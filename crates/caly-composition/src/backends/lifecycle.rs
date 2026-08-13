//! Core lifecycle builders: kernel process owners with config-driven
//! controller endpoints, rendered bootstrap configs and runtime tuning.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use caly_backends::{
    CoreLifecycleBackend, HttpSubscriptionBackend, MihomoLifecycleBackend,
    SharedCoreLifecycleBackend,
};

use super::{kernel_binary, lifecycle_support};
use crate::{rule_provider_materialize, CompositionError};

/// TUN interface name shared by the platform owner and rendered inbounds.
pub(super) const TUN_INTERFACE: &str = "caly0";

pub(super) fn build_dual_core_lifecycle(
    subscription: &mut HttpSubscriptionBackend,
    configured_core: caly_domain::CoreKind,
    secret: Option<String>,
    controllers: &caly_domain::Controllers,
    binaries: &crate::CoreBinaryPaths,
    tuning: &crate::RuntimeTuning,
    tun: Option<caly_domain::TunConfig>,
    active_cell: caly_backends::dual::SharedActiveCore,
) -> Result<caly_backends::dual::DualCoreLifecycle, CompositionError> {
    if configured_core == caly_domain::CoreKind::Xray {
        return Err(CompositionError::UnsupportedCore);
    }
    let mihomo_binary = kernel_binary(binaries.mihomo.clone(), "CALY_MIHOMO_BIN", "mihomo");
    let sing_box_binary = kernel_binary(binaries.sing_box.clone(), "CALY_SINGBOX_BIN", "sing-box");
    // Materialise every `Inline` rule provider to a real file under the
    // Mihomo work dir before the MIHOMO rendering pipeline sees them: mihomo
    // does not understand `type: inline` (caly-private extension),
    // so the payload must be on disk before rendering — a pure shape: the
    // same files and in-memory providers on every re-run. sing-box walks a
    // different path (#62): it keeps the ORIGINAL providers, because its
    // rule-set renderer converts inline payloads to real sing-box inline
    // rule-sets in `caly_coreconf::rules` (pre-#62 the materialised Clash
    // payload file was handed over as a `local` + `format: source` rule-set
    // — a Clash YAML sing-box cannot parse — breaking sing-box boot).
    let mihomo_work = work_dir("mihomo");
    let (rewritten_providers, _materialized) =
        rule_provider_materialize::materialize_inline_providers(
            &mihomo_work,
            &tuning.rule_providers,
        )
        .map_err(|error| {
            tracing::error!(?error, "inline rule provider materialization failed");
            CompositionError::BackendUnavailable
        })?;
    let mut materialized_tuning = tuning.clone();
    materialized_tuning.rule_providers = rewritten_providers;
    // The inner builder already logs the precise failure site; pass its
    // variant through instead of flattening it into BackendUnavailable.
    let mihomo = build_mihomo_lifecycle(
        mihomo_binary,
        mihomo_work,
        secret.clone(),
        controllers,
        &materialized_tuning,
        tun.clone(),
    )?;
    let sing_box = build_sing_box_lifecycle(
        subscription,
        sing_box_binary,
        work_dir("sing-box"),
        secret,
        controllers,
        tuning,
        tun,
    )?;
    Ok(caly_backends::dual::DualCoreLifecycle::new(
        active_cell,
        mihomo,
        sing_box,
    ))
}

/// Core working directories live under the XDG runtime root by default, with
/// `CALY_MIHOMO_DIR` kept as an explicit override.
fn work_dir(kind: &str) -> PathBuf {
    std::env::var_os("CALY_MIHOMO_DIR").map_or_else(
        || {
            caly_platform::paths::AppPaths::from_env()
                .core_work_dir()
                .join(kind)
        },
        PathBuf::from,
    )
}

fn build_sing_box_lifecycle(
    subscription: &mut HttpSubscriptionBackend,
    binary: PathBuf,
    working_directory: PathBuf,
    secret: Option<String>,
    controllers: &caly_domain::Controllers,
    tuning: &crate::RuntimeTuning,
    tun: Option<caly_domain::TunConfig>,
) -> Result<SharedCoreLifecycleBackend, CompositionError> {
    // Single config source of truth: same as the Mihomo path, the core loads
    // the file `config apply` would publish and a committed generation is
    // picked up on restart. Existing files survive daemon reboots.
    let config = caly_platform::paths::AppPaths::from_env()
        .config
        .join("sing-box.json");
    let controller = controllers.sing_box.clone();
    if let Some(parent) = config.parent() {
        ensure_directory(parent, "the sing-box config directory")?;
    }
    // The core work directory hosts geoip/geosite data and sing-box state; it
    // must exist even when a committed config is reused, or `run` exits before
    // the controller becomes ready.
    ensure_directory(&working_directory, "the sing-box working directory")?;
    if config.is_file() {
        // Re-key the reused generation for the rotated daemon secret.
        if let Some(secret) = &secret {
            lifecycle_support::refresh_config_secret(&config, secret)?;
        }
    } else {
        let json = render_sing_box_bootstrap(
            subscription,
            controller.clone(),
            secret.as_deref(),
            tuning,
            tun.as_ref(),
        )?;
        lifecycle_support::publish_owner_only_config(&config, json, 1)?;
    }
    let backend = caly_backends::SingBoxLifecycleBackend::new(
        binary.clone(),
        working_directory,
        controller,
        config,
        1,
        secret,
        tun.is_some(),
    )
    .map_err(|error| {
        tracing::error!(?error, binary = ?binary, "cannot build the sing-box lifecycle (is the binary installed and executable?)");
        CompositionError::BackendUnavailable
    })?;
    Ok(wrap_lifecycle(
        CoreLifecycleBackend::SingBox(Arc::new(Mutex::new(backend))),
        tuning,
    ))
}

/// Creates a directory, mapping the failure to `BackendUnavailable` with the
/// site-specific log line.
fn ensure_directory(path: &std::path::Path, what: &str) -> Result<(), CompositionError> {
    std::fs::create_dir_all(path).map_err(|error| {
        tracing::error!(?error, path = ?path, "cannot create {what}");
        CompositionError::BackendUnavailable
    })
}

/// Wraps a concrete lifecycle backend in the shared, timeout-bounded handle.
fn wrap_lifecycle(
    backend: CoreLifecycleBackend,
    tuning: &crate::RuntimeTuning,
) -> SharedCoreLifecycleBackend {
    SharedCoreLifecycleBackend::with_timeouts(
        Arc::new(Mutex::new(backend)),
        std::time::Duration::from_millis(tuning.start_timeout_ms),
        std::time::Duration::from_millis(tuning.stop_timeout_ms),
    )
}

/// Renders the sing-box bootstrap document: the `CALY_SUBSCRIPTION_URL`
/// subscription when configured, otherwise the subscription-less base config.
fn render_sing_box_bootstrap(
    subscription: &mut HttpSubscriptionBackend,
    controller: String,
    secret: Option<&str>,
    tuning: &crate::RuntimeTuning,
    tun: Option<&caly_domain::TunConfig>,
) -> Result<Vec<u8>, CompositionError> {
    let render_tuning = sing_box_render_tuning(controller.clone(), secret, tuning, tun.cloned());
    let json = if let Some(url) = std::env::var_os("CALY_SUBSCRIPTION_URL") {
        // Audit #113: derive the id from the URL digest (like the refresh
        // path) so the bootstrap source and later refreshes share ONE cache
        // identity instead of a stale pseudo-id twin.
        let subscription_id =
            caly_backends::subscription::subscription_id_for_url(&url.to_string_lossy());
        subscription.put_url(subscription_id, url.to_string_lossy().into_owned());
        // The bootstrap runs inside the daemon Tokio runtime; the
        // subscription backend performs blocking HTTP via its captured
        // handle, which panics on a nested runtime. `block_in_place`
        // releases the worker so the blocking fetch is legal here.
        tokio::task::block_in_place(|| {
            subscription.refresh_sing_box_config(subscription_id, &render_tuning)
        })
        .map_err(|error| {
            tracing::error!(
                ?error,
                "subscription fetch or render for the sing-box bootstrap failed"
            );
            CompositionError::BackendUnavailable
        })?
    } else {
        render_base_sing_box_config(&controller, secret, tuning, tun)?
    };
    Ok(json)
}

/// Builds the sing-box render tuning from the runtime tuning.
fn sing_box_render_tuning(
    controller: String,
    secret: Option<&str>,
    tuning: &crate::RuntimeTuning,
    tun: Option<caly_domain::TunConfig>,
) -> caly_coreconf::sing_box::SingBoxRenderTuning {
    // The bootstrap path predates the author-group render (the document
    // renderer composes its own outbounds), so no named group outbounds
    // exist here yet: policies stay on the PROXY selector.
    let rendered_rules = caly_coreconf::rules::render_sing_box_rules(
        &tuning.rules,
        &tuning.rule_providers,
        true,
        &std::collections::BTreeSet::new(),
    );
    caly_coreconf::sing_box::SingBoxRenderTuning {
        controller,
        secret: secret.unwrap_or_default().to_owned(),
        log_level: tuning.log_level.clone(),
        mixed_port: tuning.mixed_port,
        allow_lan: tuning.allow_lan,
        bind_address: tuning.bind_address.clone(),
        tun,
        tun_interface: TUN_INTERFACE.to_owned(),
        dns: tuning.dns.clone(),
        sniff: tuning.sniffer.enabled,
        sniff_override_destination: tuning.sniffer.override_destination,
        // Subscription documents carry proxy outbounds, so every rule policy
        // is representable (group names fold into the PROXY selector).
        route_rules: rendered_rules.rules,
        rule_sets: rendered_rules.rule_sets,
        route_final: rendered_rules.final_outbound,
        block_outbound: rendered_rules.block_outbound,
    }
}

/// Renders the subscription-less sing-box config with config-driven tuning
/// (DNS, mixed inbound, transparent inbound, sniffing, optional TUN, and the
/// config-driven route rules that remain representable without proxy nodes).
fn render_base_sing_box_config(
    controller: &str,
    secret: Option<&str>,
    tuning: &crate::RuntimeTuning,
    tun: Option<&caly_domain::TunConfig>,
) -> Result<Vec<u8>, CompositionError> {
    // Without subscription nodes no proxy/group policy is representable; such
    // rules are skipped (counted) instead of fabricating outbounds.
    let rendered_rules = caly_coreconf::rules::render_sing_box_rules(
        &tuning.rules,
        &tuning.rule_providers,
        false,
        &std::collections::BTreeSet::new(),
    );
    if rendered_rules.skipped > 0 {
        tracing::warn!(
            skipped = rendered_rules.skipped,
            "sing-box base config has no proxy outbounds; {} routing rule(s) \
             targeting proxy policies were skipped (refresh a subscription to \
             apply them)",
            rendered_rules.skipped
        );
    }
    let base = caly_coreconf::sing_box::SingBoxBaseTuning {
        controller: controller.to_owned(),
        dns: tuning.dns.clone(),
        secret: secret.map(str::to_owned),
        log_level: tuning.log_level.clone(),
        mixed_port: tuning.mixed_port,
        allow_lan: tuning.allow_lan,
        bind_address: tuning.bind_address.clone(),
        tun: tun.cloned(),
        tun_interface: TUN_INTERFACE.to_owned(),
        transparent_port: if tuning.transparent.enabled {
            tuning.transparent.port
        } else {
            0
        },
        transparent_tproxy: matches!(
            tuning.transparent.mode,
            caly_profile::schema::TransparentMode::Tproxy
        ),
        sniff: caly_coreconf::sing_box::SniffOptions {
            enabled: tuning.sniffer.enabled,
            override_destination: tuning.sniffer.override_destination,
        },
        route_rules: rendered_rules.rules,
        rule_sets: rendered_rules.rule_sets,
        route_final: rendered_rules.final_outbound,
        block_outbound: rendered_rules.block_outbound,
    };
    caly_coreconf::sing_box::SingBoxConfigRenderer
        .render_tuned(&base)
        .map_err(|error| {
            tracing::error!(?error, "sing-box config render failed");
            CompositionError::BackendUnavailable
        })
}

fn build_mihomo_lifecycle(
    binary: PathBuf,
    working_directory: PathBuf,
    secret: Option<String>,
    controllers: &caly_domain::Controllers,
    tuning: &crate::RuntimeTuning,
    tun: Option<caly_domain::TunConfig>,
) -> Result<SharedCoreLifecycleBackend, CompositionError> {
    // Single config source of truth: the core loads the same file `config
    // apply` publishes, so a committed generation reaches the running kernel
    // on the next restart. A prior apply file is reused across daemon boots;
    // otherwise a base generation is rendered and published here.
    let config = caly_platform::paths::AppPaths::from_env()
        .config
        .join("mihomo.yaml");
    let controller = controllers.mihomo.clone();
    let lifecycle_secret = secret.clone();
    ensure_directory(&working_directory, "Mihomo working directory")?;
    if let Some(parent) = config.parent() {
        ensure_directory(parent, "the Mihomo config directory")?;
    }
    // Reuse a previously applied generation but re-key its controller secret
    // so a restarted daemon (which rotates the secret) can authenticate.
    if config.is_file() {
        if let Some(secret) = &secret {
            lifecycle_support::refresh_config_secret(&config, secret)?;
        }
    } else {
        lifecycle_support::render_mihomo_bootstrap(
            &config,
            &controller,
            secret,
            tuning,
            tun.clone(),
        )?;
    }
    let backend = MihomoLifecycleBackend::new(
        binary.clone(),
        working_directory.clone(),
        controller,
        config,
        1,
        lifecycle_secret,
        tun.is_some(),
    )
    .map_err(|error| {
        tracing::error!(?error, binary = ?binary, "cannot build Mihomo lifecycle (is the binary installed and executable?)");
        CompositionError::BackendUnavailable
    })?;
    Ok(wrap_lifecycle(
        CoreLifecycleBackend::Mihomo(Arc::new(Mutex::new(backend))),
        tuning,
    ))
}
