//! Daemon runtime assembly: Tokio runtime, UDS/TCP transport, lock and cleanup.
//!
//! Kept as a sibling of `main.rs` so the composition root stays within the
//! project's file-size limit.

use std::{path::PathBuf, process::ExitCode};

use caly_domain::DaemonInstanceId;
use caly_platform::instance_lock::{
    InstanceLock, InstanceLockBackend, LinuxInstanceLockBackend, LockOwner,
    current_process_start_id,
};
use caly_protocol::protocol::v2::{DecodeLimits, all_features};
use caly_server::{json::ServiceAdapter, uds::serve_owner_only_until};

/// Owns the shared daemon Tokio runtime and the running application tasks.
pub(crate) struct DaemonRuntime {
    runtime: tokio::runtime::Runtime,
    pub(crate) running: caly_composition::RunningApplication,
    pub(crate) daemon_instance: [u8; 16],
    pub(crate) socket: PathBuf,
    pub(crate) listen: Option<std::net::SocketAddr>,
    /// Handshake admission token (`daemon.auth_token`, #58).
    pub(crate) auth_token: Option<String>,
    /// TLS material paths for the TCP listener (#57); `None`
    /// keeps the listener plaintext (loopback trust model).
    pub(crate) tls_material: Option<caly_server::tcp::TlsMaterialPaths>,
    /// Periodic subscription refresh cadence (#59); `None`
    /// disables the background timer.
    pub(crate) subscription_refresh: Option<std::time::Duration>,
}

pub(crate) fn start_daemon_runtime(
    application: caly_composition::ApplicationComposition,
    daemon_instance: DaemonInstanceId,
    socket: PathBuf,
    listen: Option<std::net::SocketAddr>,
    auth_token: Option<String>,
    tls_material: Option<caly_server::tcp::TlsMaterialPaths>,
    subscription_refresh: Option<std::time::Duration>,
    auto_start_core: bool,
) -> Result<DaemonRuntime, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        // Two workers is enough for an I/O-bound daemon (UDS clients,
        // watch streams, subscription fetch, kernel HTTP control): the
        // pre-change default spawned one worker per CPU core, so an
        // idle daemon on a 4-core box parked 4 worker threads + stacks
        // (~2 MB each) for no throughput gain.
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| format!("daemon runtime creation failed: {error}"))?;
    let running = runtime.block_on(async {
        application
            .start_runtime(auto_start_core)
            .map_err(|error| format!("application runtime failed: {error}"))
    })?;
    Ok(DaemonRuntime {
        runtime,
        running,
        daemon_instance: daemon_instance.into_bytes(),
        socket,
        listen,
        auth_token,
        tls_material,
        subscription_refresh,
    })
}

async fn wait_for_fatal(
    mut receiver: tokio::sync::watch::Receiver<Option<caly_application::runtime::FatalFault>>,
) {
    if receiver.borrow().is_some() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if receiver.borrow_and_update().is_some() {
            return;
        }
    }
}

/// The "first finisher" payload of the UDS-vs-TCP `select!` in
/// `serve`. Hoisted to module scope so the statement flow of `serve`
/// stays linear (clippy `items_after_statements`).
enum Primary {
    Uds(Result<(), Box<dyn std::error::Error + Send + Sync>>),
    Tcp(Result<Result<(), Box<dyn std::error::Error + Send + Sync>>, tokio::task::JoinError>),
}

impl DaemonRuntime {
    pub(crate) fn serve(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Refuse to bind over a regular file, symlink, or another user's socket.
        caly_platform::uds::validate_uds_path(&self.socket).map_err(|reason| {
            std::io::Error::other(format!(
                "refusing to bind unsafe UDS path {}: {reason:?}",
                self.socket.display()
            ))
        })?;
        // Admission: when `daemon.auth_token` is configured
        // EVERY transport demands it — the UDS transport is
        // already guarded by socket permissions + peer
        // credentials, but a shared adapter keeps the two
        // transports on one policy so the TCP listener can never
        // drift into the weaker posture (#58).
        let adapter = ServiceAdapter::new(
            self.running.service_handle(),
            self.daemon_instance,
            session_token_for(self.daemon_instance),
            caly_server::admission::TokenAdmission::from_configured(self.auth_token.clone()),
            all_features(),
            DecodeLimits::v2_default(),
        );
        let socket = self.socket.clone();
        let fatal = self.running.fatal_receiver().map_err(|error| {
            std::io::Error::other(format!("runtime fatal subscription failed: {error}"))
        })?;
        // Serve the owner-only UDS plus the configured loopback TCP transport
        // concurrently. Both share one `JsonService` (and thus the daemon
        // session registry); a shared notify stops both on a runtime fatal.
        //
        // Round 17: the `stop_notifier` is the same `Notify`
        // as `fatal_shutdown` — both signal "the serve loop
        // must end, then `shutdown()` cleans up". A
        // successful `StopDaemon` operation triggers the
        // notifier (clean exit); a fatal fault does the
        // same. The transport's `shutdown` future is
        // `fatal_shutdown.notified()` (so a fatal or a
        // successful Stop both break the serve loop).
        let stop_notifier = std::sync::Arc::new(tokio::sync::Notify::new());
        let grpc = caly_server::json::JsonService::new(adapter, Some(stop_notifier.clone()))
            .map_err(|error| {
                std::io::Error::other(format!("session registry init failed: {error}"))
            })?;
        let notify_task = stop_notifier.clone();
        self.runtime.spawn(async move {
            wait_for_fatal(fatal).await;
            notify_task.notify_waiters();
        });
        // #59: opt-in periodic subscription refresh. The timer
        // submits the SAME `RefreshSubscription` command the
        // `caly sub refresh` RPC submits, through the same
        // bounded admission path — the SubscriptionActor owns
        // the actual fetch/merge, the timer is a pure scheduler.
        // The first immediate tick is skipped so the daemon does
        // not fetch at boot (the operator's config continuity is
        // restored from cache on boot already).
        if let Some(interval) = self.subscription_refresh {
            let service = self.running.service_handle();
            let stop = stop_notifier.clone();
            self.runtime.spawn(async move {
                let mut tick = tokio::time::interval(interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                tick.tick().await; // consume the immediate first tick
                loop {
                    tokio::select! {
                        () = stop.notified() => break,
                        _ = tick.tick() => submit_periodic_refresh(&service),
                    }
                }
            });
        }
        self.runtime.block_on(async {
            let uds_grpc = grpc.clone();
            let uds_shutdown = stop_notifier.clone();
            let uds = async move {
                let shutdown = async move { uds_shutdown.notified().await };
                serve_owner_only_until(socket.clone(), uds_grpc, shutdown).await
            };
            if let Some(listen) = self.listen {
                let tcp_grpc = grpc.clone();
                let tcp_shutdown = stop_notifier.clone();
                let tls_material = self.tls_material.clone();
                let tcp = async move {
                    let shutdown = async move { tcp_shutdown.notified().await };
                    match &tls_material {
                        // TLS material resolved at boot: serve
                        // the listener through rustls (#57).
                        Some(material) => {
                            caly_server::tcp::serve_tls_tcp_until(
                                listen, material, tcp_grpc, shutdown,
                            )
                            .await
                        }
                        None => caly_server::tcp::serve_tcp_until(listen, tcp_grpc, shutdown).await,
                    }
                };
                let mut tcp_task = tokio::spawn(tcp);
                tokio::pin!(uds);
                // The UDS channel is the primary client transport; the TCP
                // listener is an add-on. The old `select!` returned whichever
                // side finished first, so a TCP bind failure (port occupied)
                // used to tear down the whole daemon. Degrade instead: a
                // TCP-side end logs a warning and UDS keeps serving.
                let primary = tokio::select! {
                    result = &mut uds => Primary::Uds(result),
                    joined = &mut tcp_task => Primary::Tcp(joined),
                };
                match primary {
                    Primary::Uds(result) => {
                        tcp_task.abort();
                        result
                    }
                    Primary::Tcp(joined) => {
                        match joined {
                            Ok(Ok(())) => {
                                tracing::warn!(
                                    "TCP listener stopped; daemon continues on UDS only"
                                );
                            }
                            Ok(Err(error)) => tracing::warn!(
                                "TCP listener failed: {error}; daemon continues on UDS only"
                            ),
                            Err(error) => tracing::warn!(
                                "TCP listener task aborted: {error}; daemon continues on UDS only"
                            ),
                        }
                        uds.await
                    }
                }
            } else {
                uds.await
            }
        })
    }

    pub(crate) fn shutdown(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let first_fault = self.running.first_fault();
        let _ = self.running.stop_core();
        self.runtime
            .block_on(self.running.shutdown())
            .map_err(|error| {
                format!("daemon shutdown failed: {error:?}; first runtime fault: {first_fault:?}")
                    .into()
            })
    }
}

pub(crate) fn acquire_lock(lock_path: PathBuf) -> Result<Box<dyn InstanceLock>, String> {
    let process_start_id = current_process_start_id().map_err(|error| error.to_string())?;
    // A fresh random owner token per attempt keeps the lock file's owner
    // identity unpredictable even if the PID/start pair were ever replayed.
    let owner_token = caly_platform::entropy::random_bytes::<16>();
    LinuxInstanceLockBackend
        .acquire(
            lock_path,
            LockOwner {
                pid: std::process::id(),
                process_start_id,
                owner_token,
            },
        )
        // Render the structured failure as a human sentence; the raw Debug
        // form leaks internal types into user-facing diagnostics.
        .map_err(|error| {
            format!(
                "{} ({}); hint: {}",
                error.message, error.resource, error.suggested_action
            )
        })
}

pub(crate) fn bootstrap_ready(assembly: &mut crate::bootstrap::DaemonAssembly) -> bool {
    for phase in [
        crate::bootstrap::BootstrapPhase::AcquireInstanceLock,
        crate::bootstrap::BootstrapPhase::RestorePendingEffects,
        crate::bootstrap::BootstrapPhase::BuildOwnedRuntime,
        crate::bootstrap::BootstrapPhase::BindTransport,
    ] {
        if let Err(error) = assembly.bootstrap.complete(phase) {
            tracing::error!(phase = ?phase, "daemon bootstrap failed: {error:?}");
            return false;
        }
    }
    true
}

pub(crate) fn finish_daemon(
    result: Result<(), Box<dyn std::error::Error + Send + Sync>>,
    lock: Box<dyn InstanceLock>,
    socket: &std::path::Path,
    first_fault: Option<caly_application::runtime::FatalFault>,
) -> ExitCode {
    // W3a 诊断: a fatal that stopped the daemon must be visible even on the
    // clean-exit path (serve() returns Ok after a fatal notify; without this
    // the operator sees exit 0 and a silent death).
    if let Some(fault) = first_fault {
        tracing::error!(?fault, "daemon exiting after a runtime fatal");
    } else {
        tracing::info!("daemon serve loop ended (stop command or OS signal)");
    }
    let socket_result = std::fs::remove_file(socket);
    // Best-effort cleanup of the owner-only controller-secret file so a stopped
    // daemon never leaves a stale secret for offline queries to read.
    let _ =
        std::fs::remove_file(caly_platform::paths::AppPaths::from_env().controller_secret_path());
    let release_result = lock.release();
    match (result, socket_result, release_result) {
        (Ok(()), Ok(()), Ok(())) => ExitCode::SUCCESS,
        // A stale-socket removal failure is not a daemon failure: the socket
        // may already be gone (removed by a previous run) or unlinked by the
        // operator. Surface it, still exit clean.
        (Ok(()), Err(error), Ok(())) => {
            tracing::warn!("could not remove the UDS socket: {error}");
            ExitCode::SUCCESS
        }
        (Err(error), _, _) => {
            tracing::error!("daemon transport failed: {error}");
            ExitCode::FAILURE
        }
        (Ok(()), _, Err(error)) => {
            tracing::error!("daemon lock release failed: {error:?}");
            ExitCode::FAILURE
        }
    }
}

/// Derives the handshake session token. The pre-fix daemon
/// reused the daemon-instance id as the session token, so the
/// bearer credential was forgeable from publicly observable
/// handshake response bytes alone (#28): the daemon mints (and
/// persists at 0600 in the state root) a random secret, and
/// the session token is the first half of `SHA-256(secret)`.
/// Any process can still complete a handshake to *learn* the
/// token (that is the admission model), but it can no longer
/// be constructed offline from the instance id, and it rotates
/// whenever the state root is reset instead of being a global
/// constant. Session admission on the TCP listener is further
/// hardened by `daemon.auth_token` (#58); UDS is guarded by
/// peer credentials (#27).
fn session_token_for(daemon_instance: [u8; 16]) -> [u8; 16] {
    use sha2::Digest;
    let secret = session_secret().unwrap_or(daemon_instance);
    let digest = sha2::Sha256::digest(secret);
    let mut token = [0u8; 16];
    token.copy_from_slice(&digest[..16]);
    token
}

/// Loads or mints the daemon session secret at
/// `<state>/daemon-session.key` with owner-only permissions
/// (0600), so accidental world-readable copies of the state
/// root never leak it. Returns `None` only when the state root
/// is unusable; the caller then falls back to the
/// daemon-instance id (pre-#28 behaviour — that fallback keeps
/// the daemon functional on systems where the state root is
/// already broken, which every daemon command would hit too).
fn session_secret() -> Option<[u8; 16]> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = caly_platform::paths::AppPaths::from_env()
        .state
        .join("daemon-session.key");
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() >= 16 => {
            // Audit #82: the key file must stay owner-only; repair a loose
            // mode best-effort instead of serving a world-readable secret.
            if let Ok(metadata) = std::fs::metadata(&path) {
                use std::os::unix::fs::PermissionsExt;
                let mode = metadata.permissions().mode() & 0o777;
                if mode != 0o600 {
                    tracing::warn!(
                        mode = format_args!("{mode:o}"),
                        "session key file has loose permissions; tightening to 0600"
                    );
                    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                }
            }
            let mut secret = [0u8; 16];
            secret.copy_from_slice(&bytes[..16]);
            return Some(secret);
        }
        Ok(bytes) => {
            // Audit #82: a short/truncated key file must not be silently
            // swapped for a new secret without a trace.
            tracing::warn!(
                len = bytes.len(),
                "session key file is truncated; minting a fresh secret"
            );
        }
        Err(_) => {}
    }
    // Audit #82: the previous mint derived every byte from the current unix
    // millis (`(seed % 251) ^ …`), collapsing the search space to ~8 bits.
    // Mint from the kernel CSPRNG instead and fail closed (None keeps the
    // caller's daemon-instance fallback, with identical pre-#28 semantics).
    let secret = match caly_platform::entropy::try_random_bytes::<16>() {
        Ok(value) => value,
        Err(reason) => {
            tracing::warn!(%reason, "kernel CSPRNG unavailable for session secret; keeping instance-id fallback");
            return None;
        }
    };
    let parent = path.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    // `create_new`: two racing daemons must never overwrite each other's
    // freshly minted secret mid-handshake; a loser simply reads the winner's.
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path);
    match opened {
        Ok(mut file) => {
            if file.write_all(&secret).is_err() {
                return None;
            }
            Some(secret)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Lost the creation race: adopt the file another daemon wrote.
            let bytes = std::fs::read(&path).ok()?;
            if bytes.len() < 16 {
                return None;
            }
            let mut adopted = [0u8; 16];
            adopted.copy_from_slice(&bytes[..16]);
            Some(adopted)
        }
        Err(_) => None,
    }
}

fn current_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod session_token_tests;

/// One periodic-refresh submission (#59). The subscription id
/// is a fixed zero placeholder: the current
/// `SubscriptionCommandHandler` refreshes EVERY enabled source
/// regardless of the id (the id exists for the wire RPC's
/// per-source future). Failure here only logs — the next tick
/// retries; a rejected submission (e.g. the admission queue is
/// momentarily full) is deliberately not retried inline so the
/// timer never piles up work.
fn submit_periodic_refresh<S>(service: &std::sync::Arc<std::sync::Mutex<S>>)
where
    S: caly_application::service::ApplicationServicePort,
{
    let envelope = caly_application::command_bus::CommandEnvelope {
        operation_id: periodic_operation_id(),
        command: caly_application::command_bus::Command::RefreshSubscription {
            subscription_id: caly_domain::SubscriptionId::from_bytes([0u8; 16]),
            // W2-β2b: the periodic timer submits a *scheduled* run —
            // sources pinned static (`refresh_every_minutes: 0`)
            // are skipped and per-source cadences gate the rest.
            force: false,
            scheduled: true,
        },
    };
    let outcome = service.lock().map(|mut svc| svc.submit(envelope));
    match outcome {
        Ok(Ok(_)) => tracing::info!("periodic subscription refresh submitted"),
        Ok(Err(error)) => tracing::warn!("periodic subscription refresh rejected: {error:?}"),
        Err(_) => tracing::warn!("periodic subscription refresh: service lock poisoned"),
    }
}

/// Unique-ish operation id for one tick: the unix-millis value
/// little-endian in the first 8 bytes, with a marker tag so
/// periodic operations are recognizable in `op list`.
fn periodic_operation_id() -> caly_domain::OperationId {
    // Audit #81: `b"caly.tick"` is 9 bytes and `bytes[8..]` is exactly 8;
    // the pre-fix `copy_from_slice` panicked on every tick.
    const PERIODIC_TAG: [u8; 8] = *b"caly.tik";
    let mut bytes = [0u8; 16];
    let millis = current_unix_millis();
    bytes[..8].copy_from_slice(&millis.to_le_bytes());
    bytes[8..].copy_from_slice(&PERIODIC_TAG);
    caly_domain::OperationId::from_bytes(bytes)
}

#[cfg(test)]
mod runtime_tests;
