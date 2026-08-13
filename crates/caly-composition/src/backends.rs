use std::path::PathBuf;

use caly_backends::{HttpSubscriptionBackend, LinuxSystemProxyBackend, TemplateRenderBackend};
use caly_platform::paths::AppPaths;

use super::CompositionError;

mod lifecycle;
mod lifecycle_support;

/// Bounds a controller secret for the core control clients. A generated hex
/// secret always fits; the clamped constructor keeps this infallible.
fn bounded_text(value: &str) -> caly_domain::BoundedText<4_096> {
    caly_domain::BoundedText::from_nonempty_clamped(value.to_owned(), "invalid")
}

/// Builds both core command adapters (Mihomo + sing-box) sharing one node
/// registry, so a runtime core switch keeps the subscription index intact.
fn build_dual_core_backend(
    controllers: &caly_domain::Controllers,
    secret: &str,
    active_cell: caly_backends::dual::SharedActiveCore,
) -> Result<
    (
        caly_backends::dual::SwitchableCoreBackend,
        caly_backends::CoreNodeRegistry,
    ),
    CompositionError,
> {
    let secret_text = bounded_text(secret);
    let mihomo_control = caly_corectl::mihomo::MihomoHttpControl::new(
        controllers.mihomo.clone(),
        Some(secret_text.clone()),
    )
    .map_err(|error| {
        tracing::error!(?error, "cannot build the Mihomo control client");
        CompositionError::BackendUnavailable
    })?;
    let mihomo = caly_backends::MihomoCoreBackend::new(mihomo_control);
    let registry = mihomo.registry();
    let sing_box_control = caly_corectl::sing_box::SingBoxHttpControl::new(
        controllers.sing_box.clone(),
        Some(secret_text),
    )
    .map_err(|error| {
        tracing::error!(?error, "cannot build the sing-box control client");
        CompositionError::BackendUnavailable
    })?;
    let sing_box =
        caly_backends::SingBoxCoreBackend::new_with_registry(sing_box_control, registry.clone());
    Ok((
        caly_backends::dual::SwitchableCoreBackend::new(mihomo, sing_box, active_cell),
        registry,
    ))
}

pub(crate) struct RuntimeBackends {
    pub(crate) core: caly_backends::dual::SwitchableCoreBackend,
    pub(crate) subscription: HttpSubscriptionBackend,
    pub(crate) config: caly_backends::config::ActiveConfigBackend,
    pub(crate) platform:
        caly_backends::platform::DurableSystemProxyBackend<LinuxSystemProxyBackend>,
    pub(crate) tun:
        caly_backends::platform::DurableTunBackend<caly_backends::LinuxTunCommandBackend>,
    pub(crate) lifecycle: caly_backends::dual::DualCoreLifecycle,
    pub(crate) template: TemplateRenderBackend,
    /// Shared node registry (node_id → registered proxy), consumed by the
    /// selection reconciler to follow renames after subscription refreshes.
    pub(crate) registry: caly_backends::CoreNodeRegistry,
    /// Shared controller auth secret, also used by the telemetry control client.
    pub(crate) controller_secret: Option<String>,
}

pub(super) fn build_runtime_backends(
    configured_core: caly_domain::CoreKind,
    tun: Option<caly_domain::TunConfig>,
    controllers: &caly_domain::Controllers,
    binaries: &super::CoreBinaryPaths,
    subscription_urls: &[String],
    tuning: &super::RuntimeTuning,
) -> Result<RuntimeBackends, CompositionError> {
    // A shared, fail-closed controller auth secret. It is embedded in the core
    // config and sent by the control clients; it is never part of the
    // projection or any PresentationSnapshot.
    let secret = caly_platform::secrets::generate_secret_hex().map_err(|error| {
        tracing::error!(?error, "cannot generate the controller auth secret");
        CompositionError::SecretUnavailable
    })?;
    // The active-core cell is created here and shared by the lifecycle,
    // command and telemetry adapters so a runtime switch is atomic everywhere.
    let active_cell: caly_backends::dual::SharedActiveCore =
        std::sync::Arc::new(std::sync::Mutex::new(configured_core));
    let (core, registry) = build_dual_core_backend(controllers, &secret, active_cell.clone())
        .inspect_err(|error| tracing::error!("build_dual_core_backend failed: {error:?}"))?;
    // Subscription-author routing surface (2026-08-09 规划): populated by
    // subscription refresh alongside the node registry, consumed by the
    // mihomo config backend so author-declared groups render verbatim.
    let routing = caly_backends::shared_routing_registry();
    let mut subscription = HttpSubscriptionBackend::new()
        .inspect_err(|error| tracing::error!("subscription backend failed: {error:?}"))
        .map_err(|_| CompositionError::BackendUnavailable)?
        .with_policy(tuning.fetch_policy)
        // Persist fetched bodies so the node registry survives a daemon
        // restart without a re-fetch; restored in `start_runtime`.
        .with_cache_dir(AppPaths::from_env().state.join("subscriptions"))
        .with_node_registry(registry.clone())
        .with_routing_registry(routing.clone());
    register_subscription_urls(&mut subscription, subscription_urls);
    let proxy_backend =
        LinuxSystemProxyBackend::new(tuning.system_proxy_host.clone(), tuning.system_proxy_port)
            .map_err(|error| {
                tracing::error!(?error, "cannot build the Linux system-proxy backend");
                CompositionError::BackendUnavailable
            })?;
    let proxy_store = open_recovery_store()
        .inspect_err(|error| tracing::error!("proxy recovery store failed: {error:?}"))?;
    let platform =
        caly_backends::platform::DurableSystemProxyBackend::new(proxy_backend, proxy_store);
    let lifecycle = lifecycle::build_dual_core_lifecycle(
        &mut subscription,
        configured_core,
        Some(secret.clone()),
        controllers,
        binaries,
        tuning,
        tun.clone(),
        active_cell.clone(),
    )?;
    let template = TemplateRenderBackend::default();
    let config = build_config_owner(
        registry.clone(),
        routing,
        active_cell.clone(),
        configured_core,
        tun.clone(),
        binaries,
        tuning,
        controllers,
        &secret,
    );
    let tun = build_tun_owner(tun, open_tun_recovery_store()?, tuning.tun_escalation)?;
    Ok(RuntimeBackends {
        core,
        subscription,
        config,
        platform,
        tun,
        lifecycle,
        template,
        registry,
        controller_secret: Some(secret),
    })
}

/// Builds the durable TunActor owner with the configured MTU (or a safe default).
fn build_tun_owner(
    tun: Option<caly_domain::TunConfig>,
    store: caly_backends::platform::SharedTunRecoveryStore,
    escalation: caly_platform::tun::TunEscalation,
) -> Result<
    caly_backends::platform::DurableTunBackend<caly_backends::LinuxTunCommandBackend>,
    CompositionError,
> {
    let mtu = tun.map_or(1_500, |tun| tun.mtu());
    let interface = caly_platform::tun::InterfaceName::new(lifecycle::TUN_INTERFACE.to_owned())
        .map_err(|error| {
            tracing::error!(?error, "cannot validate the TUN interface name");
            CompositionError::BackendUnavailable
        })?;
    let inner =
        caly_backends::LinuxTunCommandBackend::new_with_escalation(interface, mtu, escalation)
            .map_err(|error| {
                tracing::error!(?error, mtu, "cannot build the Linux TUN backend");
                CompositionError::BackendUnavailable
            })?;
    Ok(caly_backends::platform::DurableTunBackend::new(
        inner, store,
    ))
}

/// Opens the owner-only durable TUN recovery store under the XDG state root.
fn open_tun_recovery_store(
) -> Result<caly_backends::platform::SharedTunRecoveryStore, CompositionError> {
    use caly_platform::recovery::{FileRecoveryStore, TunRecoveryRecord};
    let path = caly_platform::paths::AppPaths::from_env().tun_recovery_record_path();
    let store = FileRecoveryStore::<TunRecoveryRecord>::new(path).map_err(|error| {
        tracing::error!(?error, "cannot open the TUN recovery store");
        CompositionError::BackendUnavailable
    })?;
    Ok(std::sync::Arc::new(store) as caly_backends::platform::SharedTunRecoveryStore)
}

/// Opens the owner-only durable proxy recovery store under the XDG state root.
fn open_recovery_store(
) -> Result<caly_backends::platform::SharedProxyRecoveryStore, CompositionError> {
    use caly_platform::recovery::FileRecoveryStore;
    let path = caly_platform::paths::AppPaths::from_env().recovery_record_path();
    let store = FileRecoveryStore::<caly_platform::recovery::ProxyRecoveryRecord>::new(path)
        .map_err(|error| {
            tracing::error!(?error, "cannot open the system-proxy recovery store");
            CompositionError::BackendUnavailable
        })?;
    Ok(std::sync::Arc::new(store) as caly_backends::platform::SharedProxyRecoveryStore)
}

/// Registers a configured source URL for the default `refresh` subscription.
fn register_subscription_urls(subscription: &mut HttpSubscriptionBackend, urls: &[String]) {
    for url in urls {
        let id = caly_backends::subscription::subscription_id_for_url(url);
        subscription.put_url(id, url.clone());
        // The declared id set gates cache restore: only live (declared)
        // subscriptions revive their cached bodies at boot — ghosts are
        // skipped with a one-shot summary (2026-08-12 user-flow audit).
        subscription.declare_id(id);
    }
}

/// Locates a bundled development core relative to the executable before using
/// the historical current-working-directory fallback. This makes
/// `target/debug/caly daemon` work regardless of the shell's current directory.
pub(super) fn bundled_binary(name: &str) -> PathBuf {
    resolve_bundled_binary(
        name,
        std::env::current_dir().ok().as_deref(),
        std::env::current_exe().ok().as_deref(),
    )
}

/// Resolves a kernel binary: an explicit config path wins, then the
/// `CALY_*_BIN` environment override, then the bundled development binary.
pub(super) fn kernel_binary(explicit: Option<PathBuf>, env_var: &str, name: &str) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os(env_var).map(PathBuf::from))
        .unwrap_or_else(|| bundled_binary(name))
}

/// Resolves a bundled kernel binary: `vendor/bin/<name>` under the working
/// directory first, then under any ancestor of the executable path (dev runs
/// from `target/debug` still find the repo's `vendor/bin`), falling back to
/// the CWD-relative path so errors show what was attempted.
pub(super) fn resolve_bundled_binary(
    name: &str,
    cwd: Option<&std::path::Path>,
    executable: Option<&std::path::Path>,
) -> PathBuf {
    if let Some(directory) = cwd {
        let candidate = directory.join("vendor").join("bin").join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Some(executable) = executable {
        for directory in executable.ancestors() {
            let candidate = directory.join("vendor").join("bin").join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("vendor").join("bin").join(name)
}

/// Builds the config-apply owner under the XDG config root. It shares the
/// subscription proxy registry and validates every candidate against the
/// configured core's real binary before committing; the active backend is
/// selected by the configured core (sing-box is no longer a fail-fast).
fn build_config_owner(
    registry: caly_backends::CoreNodeRegistry,
    routing: caly_backends::CoreRoutingRegistry,
    active: caly_backends::dual::SharedActiveCore,
    configured_core: caly_domain::CoreKind,
    tun: Option<caly_domain::TunConfig>,
    binaries: &super::CoreBinaryPaths,
    tuning: &super::RuntimeTuning,
    controllers: &caly_domain::Controllers,
    secret: &str,
) -> caly_backends::config::ActiveConfigBackend {
    let paths = caly_platform::paths::AppPaths::from_env();
    let mihomo_port = controllers
        .mihomo
        .parse::<std::net::SocketAddr>()
        .map_or(9090, |address| address.port());
    let mut mihomo = caly_backends::MihomoConfigBackend::new(paths.config.join("mihomo.yaml"))
        .with_registry(registry.clone())
        .with_routing(routing.clone())
        .with_tun(tun.clone())
        .with_kernel(
            tuning.mixed_port,
            tuning.allow_lan,
            tuning.bind_address.clone(),
            tuning.log_level.clone(),
            mihomo_port,
            tuning.dns.clone(),
        )
        .with_secret(Some(secret.to_owned()))
        .with_rules(tuning.rules.clone())
        .with_declared_groups(tuning.declared_groups.clone())
        .with_transparent(tuning.transparent)
        .with_sniffer(tuning.sniffer.clone());
    let sing_box_port = controllers
        .sing_box
        .parse::<std::net::SocketAddr>()
        .map_or(9091, |address| address.port());
    let mut sing_box = caly_backends::config::sing_box::SingBoxConfigBackend::new(
        paths.config.join("sing-box.json"),
    )
    .with_registry(registry)
    .with_routing(routing)
    .with_tun(tun)
    .with_kernel(
        tuning.mixed_port,
        tuning.allow_lan,
        tuning.bind_address.clone(),
        tuning.log_level.clone(),
        sing_box_port,
        tuning.dns.clone(),
    )
    .with_secret(Some(secret.to_owned()))
    .with_rules(tuning.rules.clone())
    .with_declared_groups(tuning.declared_groups.clone())
    .with_sniffer(tuning.sniffer.clone());
    if configured_core == caly_domain::CoreKind::Mihomo {
        let binary = kernel_binary(binaries.mihomo.clone(), "CALY_MIHOMO_BIN", "mihomo");
        let working_directory = std::env::var_os("CALY_MIHOMO_DIR")
            .map_or_else(|| paths.core_work_dir().join("mihomo"), PathBuf::from);
        // Best-effort: seed the kind data a `mihomo -t` may need (geoip.metadb)
        // from a known-good system database when the validator would otherwise
        // attempt a network download that can hang past its timeout. Missing
        // or unusable sources are skipped silently — validation still proceeds.
        provision_geoip_metadb(&working_directory);
        mihomo = mihomo.with_validation(
            binary,
            working_directory,
            std::time::Duration::from_secs(10),
        );
    } else {
        let binary = kernel_binary(binaries.sing_box.clone(), "CALY_SINGBOX_BIN", "sing-box");
        let working_directory = std::env::var_os("CALY_SINGBOX_DIR")
            .map_or_else(|| paths.core_work_dir().join("sing-box"), PathBuf::from);
        sing_box = sing_box.with_validation(
            binary,
            working_directory,
            std::time::Duration::from_secs(10),
        );
    }
    caly_backends::config::ActiveConfigBackend::new(active, mihomo, sing_box)
}

/// Best-effort seed of a usable `geoip.metadb` into the Mihomo working
/// directory so a validated config referencing GEOIP data does not hang on
/// a network fetch (a blocked network can stall past the bounded validator
/// timeout). Every failure is silently ignored, falling back to Mihomo's own
/// behavior; a pre-existing target smaller than the 1 MiB sanity bound (a
/// stub or an aborted copy) is removed before the copy so it cannot
/// masquerade as a healthy database.
fn provision_geoip_metadb(working_directory: &std::path::Path) -> bool {
    provision_geoip_metadb_with_sources(working_directory, default_geoip_sources())
}

/// Default source list: a system Mihomo database is preferred (verified,
/// complete) over the shared caly cache from earlier test downloads.
fn default_geoip_sources() -> [std::path::PathBuf; 2] {
    [
        std::path::PathBuf::from("/etc/mihomo/geoip.metadb"),
        std::env::temp_dir().join("caly-geoip-cache/geoip.metadb"),
    ]
}

/// The testable core of `provision_geoip_metadb`. Accepts an explicit source
/// list so tests can exercise every branch without depending on whatever
/// `/etc/mihomo/geoip.metadb` happens to exist in the test environment.
fn provision_geoip_metadb_with_sources<I>(working_directory: &std::path::Path, sources: I) -> bool
where
    I: IntoIterator,
    I::Item: AsRef<std::path::Path>,
{
    let target = working_directory.join("geoip.metadb");
    if let Ok(meta) = target.metadata() {
        if meta.len() >= 1_000_000 {
            return true;
        }
        // Corrupt/stub target: never reuse it, even though `is_file()` would
        // otherwise short-circuit the copy loop below. A partial file from an
        // interrupted earlier run would otherwise block the seed indefinitely.
        let _ = std::fs::remove_file(&target);
    }
    if let Err(error) = std::fs::create_dir_all(working_directory) {
        // Seeding cannot proceed without the workdir; report loudly instead
        // of silently continuing into a guaranteed-failing copy loop.
        tracing::warn!(
            "geoip.metadb provisioning skipped: cannot create {}: {error}",
            working_directory.display()
        );
        return false;
    }
    for source in sources {
        let source = source.as_ref();
        let Ok(meta) = source.metadata() else {
            continue;
        };
        // A stub (sub-1 MiB) copy is corrupt; never reuse it.
        if meta.len() < 1_000_000 {
            continue;
        }
        if std::fs::copy(source, &target).is_ok()
            && target.metadata().map_or(0, |m| m.len()) >= 1_000_000
        {
            return true;
        }
        let _ = std::fs::remove_file(&target);
    }
    false
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bundled_binary_tests;

#[cfg(test)]
mod provision_geoip_tests;
