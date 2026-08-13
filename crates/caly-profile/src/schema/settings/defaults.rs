//! Generated default-config rendering (`config generate` content).
//!
//! Split out of `settings.rs` (audit #70 file-length budget).

use std::path::PathBuf;

use super::super::{DaemonConfig, TunConfig};
use super::ControllersConfig;
/// Base `config.yaml` content: minimal and self-validating. Feature sections
/// live in `config.d/` fragments and are deep-merged by the layered loader.
pub fn render_default_base() -> String {
    "# caly base configuration\nschema_version: 1\ncore: mihomo\n".to_owned()
}

/// Renders the generated feature-file layout. `config.yaml` stays minimal and
/// self-validating; fragments are merged later by the existing layered loader.
///
/// Commented examples always show the *complete replacement block* with
/// correct nesting, so uncommenting them can never produce a stray top-level
/// key (rejected by `deny_unknown_fields`).
pub fn render_default_config_files() -> Vec<(PathBuf, String)> {
    let mut fragments = core_and_transport_fragments();
    fragments.extend(effect_and_observability_fragments());
    fragments
}

/// Fragments for core binaries, controllers and the daemon transport.
fn core_and_transport_fragments() -> Vec<(PathBuf, String)> {
    let controllers = ControllersConfig::default();
    let daemon = DaemonConfig::default();
    let kernel = super::super::KernelConfig::default();
    vec![
        (
            PathBuf::from("config.d/10-core.yaml"),
            format!(
                "# Core binaries and local Clash-compatible controllers.\n\
                 core_binaries: {{}}\n\
                 controllers:\n\
                 \x20 mihomo: {mihomo}\n\
                 \x20 sing_box: {sing_box}\n\
                 \n\
                 # To pin core executables, replace `core_binaries: {{}}` above with:\n\
                 # core_binaries:\n\
                 #   mihomo: /opt/mihomo/mihomo\n\
                 #   sing_box: /opt/sing-box/sing-box\n",
                mihomo = controllers.mihomo,
                sing_box = controllers.sing_box,
            ),
        ),
        (
            PathBuf::from("config.d/15-kernel.yaml"),
            format!(
                "# Runtime tuning for the managed proxy kernel, rendered into the core\n\
                 # config (Mihomo mixed-port/allow-lan/log-level; sing-box mixed inbound).\n\
                 # The desktop system proxy points at mixed_port unless overridden in\n\
                 # 70-system-proxy.yaml. `restart` bounds the crash-loop self-healing\n\
                 # backoff for automatic core restarts.\n\
                 kernel:\n\
                 \x20 mixed_port: {mixed_port}\n\
                 \x20 allow_lan: false\n\
                 \x20 bind_address: '*'        # LAN bind when allow_lan: true ('*' or one interface IP)\n\
                 \x20 log_level: {log_level}       # trace | debug | info | warn | error\n\
                 \x20 start_timeout_ms: 10000      # readiness budget for start/restart\n\
                 \x20 stop_timeout_ms: 5000        # graceful stop budget\n\
                 \x20 restart:                     # crash-loop self-healing backoff\n\
                 \x20\x20\x20 initial_backoff_ms: 1000\n\
                 \x20\x20\x20 max_backoff_ms: 30000\n\
                 \x20 transparent:                 # transparent proxy (default off)\n\
                 \x20\x20\x20 enabled: false\n\
                 \x20\x20\x20 mode: redirect           # redirect | tproxy\n\
                 \x20\x20\x20 port: 7892\n",
                mixed_port = kernel.mixed_port,
                log_level = kernel.log_level,
            ),
        ),
        (
            PathBuf::from("config.d/20-daemon.yaml"),
            format!(
                "# Loopback TCP gRPC listener in addition to the UDS socket.\n\
                 # Remote (non-loopback) listen requires BOTH `tls_enabled: true` (with\n\
                 # `tls_cert_path` / `tls_key_path`) and `auth_token` set — anything\n\
                 # less is refused at boot, so an unauthenticated control plane can\n\
                 # never reach the network.\n\
                 daemon:\n\
                 \x20 listen: {listen}\n\
                 \x20 tls_enabled: false\n\
                 \x20 # tls_cert_path: /etc/caly/daemon.pem   # required when tls_enabled\n\
                 \x20 # tls_key_path: /etc/caly/daemon-key.pem\n\
                 \x20 auto_start_core: true        # engage the core at boot; a failure only warns\n",
                listen = daemon.listen,
            ),
        ),
        (
            PathBuf::from("config.d/30-subscriptions.yaml"),
            "# Public HTTP(S) subscription source(s). The SSRF policy rejects\n\
             # private/loopback destinations.\n\
             subscriptions: {}\n\
             \n\
             # To enable `caly sub refresh`, replace `subscriptions: {}` above with\n\
             # a url or a list of explicit `sources` (fetch tuning is optional;\n\
             # defaults shown). `sources` lets you refresh several providers at\n\
             # once and merge their nodes; `enabled: false` skips one source.\n\
             # subscriptions:\n\
             #   url: https://provider.example/subscription\n\
             #   sources:\n\
             #     - url: https://first.example/sub\n\
             #       enabled: true\n\
             #     - url: https://second.example/sub\n\
             #       enabled: true\n\
             #   connect_timeout_ms: 5000\n\
             #   request_timeout_ms: 30000\n\
             #   max_body_mb: 32\n\
             #   follow_redirects: false\n\
             #   use_environment_proxy: false\n\
             #\n\
             # Providers (enumeration type): when any source above is configured,\n\
             # caly auto-creates the `default` provider as an enumeration of every\n\
             # enabled source URL — `caly sub list` shows it and `sub refresh`\n\
             # fetches its sources. Bare node URIs (vmess:// …) are not sources\n\
             # (no upstream URL) and stay preview-only in `sub import`. An\n\
             # explicit `providers:` list overrides the derived default:\n\
             # providers:\n\
             #   - name: default\n\
             #     kind: subscription-sources\n"
                .to_owned(),
        ),
    ]
}

/// Fragments for platform side effects (TUN, system proxy) and observability.
fn effect_and_observability_fragments() -> Vec<(PathBuf, String)> {
    let tun = TunConfig::default();
    let mut out = vec![
        super::super::fragments_dns::dns_fragment(),
        (
            PathBuf::from("config.d/60-tun.yaml"),
            format!(
                "# TUN rendering into the core config (stack/auto-route/strict-route).\n\
                 # `caly sys tun on` additionally engages the platform TUN device and\n\
                 # requires CAP_NET_ADMIN. When the daemon lacks it, `escalation` retries\n\
                 # the privileged `ip` commands: auto = pkexec then passwordless sudo.\n\
                 tun:\n\
                 \x20 enabled: false\n\
                 \x20 mtu: {mtu}\n\
                 \x20 stack: gvisor          # gvisor | mixed | system\n\
                 \x20 auto_route: true\n\
                 \x20 strict_route: true\n\
                 \x20 escalation: auto       # auto | pkexec | sudo | none\n",
                mtu = tun.mtu,
            ),
        ),
        (
            PathBuf::from("config.d/70-system-proxy.yaml"),
            "# Desktop system proxy. Supported desktops: GNOME, KDE, niri.\n\
             # `enabled: true` engages the proxy at daemon start (degrading to a\n\
             # warning on unsupported desktops); `caly sys proxy on|off` controls it\n\
             # at runtime. The desktop state found before engagement is captured and\n\
             # restored on graceful shutdown; after a crash the effect is re-applied\n\
             # by restore-first on the next boot.\n\
             system_proxy:\n\
             \x20 enabled: false\n\
             \x20 host: 127.0.0.1\n\
             \x20 # port: 7890       # defaults to kernel.mixed_port when omitted\n"
                .to_owned(),
        ),
        (
            PathBuf::from("config.d/80-observability.yaml"),
            "# Logging and telemetry. CALY_LOG still overrides log.level.\n\
             log:\n\
             \x20 level: info          # error | warn | info | debug | trace\n\
             \n\
             telemetry:\n\
             \x20 interval_ms: 1000    # sampling cadence, >= 100\n"
                .to_owned(),
        ),
    ];
    out.push(super::super::fragments_proxy_groups::proxy_groups_fragment());
    out.push(super::super::fragments_routing::routing_fragment());
    out.push(super::super::fragments_routing::rule_providers_fragment());
    out.push(super::super::fragments_profiles::profiles_fragment());
    out
}

/// Renders one commented default `config.yaml` documenting every option, for
/// documentation and single-file setups. Composed from the exact base and
/// fragment pieces `config generate` writes, so the two layouts cannot drift.
pub fn render_default_config() -> String {
    let mut out = String::new();
    out.push_str("# caly configuration\n");
    out.push_str(
        "# Default location: $XDG_CONFIG_HOME/caly/config.yaml (usually ~/.config/caly/).\n",
    );
    out.push_str("# Managed proxy core. Values: mihomo | sing-box (xray is not supported).\n");
    out.push_str(&render_default_base());
    out.push('\n');
    for (path, contents) in render_default_config_files() {
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!("# ---- {} ----\n{contents}\n", path.display()),
        );
    }
    out
}
