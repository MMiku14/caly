//! `caly daemon` — the long-running control plane (unchanged from Round 10).

use std::process::ExitCode;

pub fn run(options: crate::cli::CliOptions) -> ExitCode {
    let paths = caly_platform::paths::AppPaths::from_env();
    // `--socket` / `CALY_SOCKET` must steer the daemon's bind path exactly
    // like they steer every client command — previously the daemon always
    // bound the default path while `show status --socket x` aimed elsewhere,
    // and the two sides never met.
    let socket_path = options.socket.clone().unwrap_or_else(|| {
        std::env::var_os("CALY_SOCKET")
            .map_or_else(|| paths.socket_path(), std::path::PathBuf::from)
    });
    let daemon_id =
        caly_domain::DaemonInstanceId::from_bytes(caly_platform::entropy::random_bytes::<16>());
    let lock = match crate::daemon::acquire_lock(paths.lock_path()) {
        Ok(value) => value,
        Err(message) => {
            tracing::error!(daemon = ?daemon_id, "daemon lock failed: {message}");
            return ExitCode::FAILURE;
        }
    };
    let mut settings = match resolve_daemon_config() {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(daemon = ?daemon_id, "daemon configuration invalid: {error}");
            let _ = lock.release();
            return ExitCode::FAILURE;
        }
    };
    if let Err(code) = apply_daemon_overrides(&mut settings, options, daemon_id) {
        let _ = lock.release();
        return code;
    }
    let mut assembly = match crate::bootstrap::DaemonAssembly::new(
        daemon_id,
        settings.core_override,
        settings.tun,
        settings.controllers,
        settings.binaries,
        settings.subscription_urls.clone(),
        settings.tuning,
    ) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(daemon = ?daemon_id, "daemon composition failed: {error:?}");
            let _ = lock.release();
            return ExitCode::FAILURE;
        }
    };
    if !crate::daemon::bootstrap_ready(&mut assembly) {
        let _ = lock.release();
        return ExitCode::FAILURE;
    }
    let daemon = match crate::daemon::start_daemon_runtime(
        assembly.application,
        daemon_id,
        socket_path,
        settings.listen,
        settings.auth_token,
        settings.tls_material,
        settings.subscription_refresh,
        settings.auto_start_core,
    ) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(daemon = ?daemon_id, "{error}");
            let _ = lock.release();
            return ExitCode::FAILURE;
        }
    };
    let socket = daemon.socket.clone();
    tracing::info!(daemon = ?daemon_id, socket = %socket.display(), listen = ?daemon.listen, "caly daemon started");
    let result = daemon.serve();
    let first_fault = daemon.running.first_fault();
    let shutdown_error = daemon.shutdown();
    crate::daemon::finish_daemon(result.and(shutdown_error), lock, &socket, first_fault)
}

fn resolve_daemon_config()
-> Result<crate::daemon_config::DaemonSettings, crate::daemon_config::DaemonConfigError> {
    crate::daemon_config::resolve_daemon(caly_platform::paths::AppPaths::from_env().config)
}

fn apply_daemon_overrides(
    settings: &mut crate::daemon_config::DaemonSettings,
    options: crate::cli::CliOptions,
    daemon_id: caly_domain::DaemonInstanceId,
) -> Result<(), ExitCode> {
    if let Some(core) = options.core.as_deref() {
        settings.core_override = match core {
            "mihomo" => Some(caly_domain::CoreKind::Mihomo),
            "sing-box" => Some(caly_domain::CoreKind::SingBox),
            _ => {
                tracing::error!(daemon = ?daemon_id, "invalid daemon core override");
                return Err(ExitCode::from(2));
            }
        };
    }
    if options.mihomo_bin.is_some() {
        settings.binaries.mihomo = options.mihomo_bin;
    }
    if options.sing_box_bin.is_some() {
        settings.binaries.sing_box = options.sing_box_bin;
    }
    Ok(())
}
