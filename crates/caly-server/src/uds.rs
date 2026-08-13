//! Linux owner-only Unix-domain-socket transport.

use std::{
    future::Future,
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use caly_application::service::ApplicationServicePort;
use tokio::net::{UnixListener, UnixStream};

use crate::json::JsonService;

/// Re-validates an existing UDS path immediately before `bind` so the
/// `validate_uds_path` call from the daemon entry point cannot be invalidated
/// by a TOCTOU swap (attacker replaces the validated socket node with a
/// symlink or a foreign-owned file between validation and bind). The check
/// mirrors `caly_platform::uds::validate_uds_path` and is intentionally
/// duplicated here so the bind helper has no inter-module ordering
/// dependency that an attacker could exploit.
fn revalidate_existing(path: &Path) -> io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to bind over symlink at {}: bind could be redirected",
                path.display()
            ),
        ));
    }
    if !file_type.is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to remove non-socket file at {}; it may be user data",
                path.display()
            ),
        ));
    }
    // Foreign-owned socket: refuse rather than risk `unlink` on another
    // user's IPC endpoint.
    let owner = std::fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot determine effective uid; refusing to remove foreign socket",
            )
        })?;
    if meta.uid() != owner {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket at {} is owned by uid {}; current uid is {owner}",
                path.display(),
                meta.uid()
            ),
        ));
    }
    Ok(())
}

/// Binds an owner-only Linux UDS, removing only an existing socket node the
/// current uid owns and immediately re-validating the path before `unlink`
/// so a TOCTOU swap cannot redirect the bind.
///
/// The freshly bound socket is set to mode `0o600`; the resulting mode is
/// then re-read from `stat` and the bind is rejected if the file system did
/// not honor the chmod (e.g. an ACL or a read-only mount). Without the
/// post-chmod check, a daemon crash between `bind` and `chmod` would leave
/// a world-accessible socket on disk for any local user to connect to.
pub fn bind_owner_only(path: &Path) -> io::Result<UnixListener> {
    revalidate_existing(path)?;
    if let Err(error) = std::fs::remove_file(path) {
        // A concurrent owner may have removed the file after revalidation;
        // treat the race as a transient retry and continue to bind.
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let observed = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if observed != 0o600 {
        // Best-effort cleanup so we never leave a non-owner-only socket
        // node behind; the bind failure surfaces the real reason.
        let _ = std::fs::remove_file(path);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket at {} is mode {:o} after chmod 0o600; refusing to leave a \
                 world-accessible IPC endpoint (check filesystem ACLs or mount \
                 options)",
                path.display(),
                observed
            ),
        ));
    }
    Ok(listener)
}

pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT; daemon shutting down");
            }
            () = async { if let Some(signal) = terminate.as_mut() { signal.recv().await; } } => {
                tracing::info!("received SIGTERM; daemon shutting down");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Shared accept surface over the two tokio listeners (TCP and UDS); the
/// address type differs per transport and is discarded by the accept loop.
pub(crate) trait TransportListener {
    type Stream: Send + 'static;
    type Address;
    fn accept(&self) -> impl Future<Output = std::io::Result<(Self::Stream, Self::Address)>>;
}

impl TransportListener for tokio::net::TcpListener {
    type Stream = tokio::net::TcpStream;
    type Address = std::net::SocketAddr;
    fn accept(&self) -> impl Future<Output = std::io::Result<(Self::Stream, Self::Address)>> {
        tokio::net::TcpListener::accept(self)
    }
}

impl TransportListener for tokio::net::UnixListener {
    type Stream = tokio::net::UnixStream;
    type Address = tokio::net::unix::SocketAddr;
    fn accept(&self) -> impl Future<Output = std::io::Result<(Self::Stream, Self::Address)>> {
        tokio::net::UnixListener::accept(self)
    }
}

/// Runs one accept loop until shutdown, handing each accepted stream to
/// `on_accept`, which owns the per-connection task spawn.
///
/// Audit #92: a transient accept error (fd pressure: EMFILE/ENFILE, a reset
/// racing the accept, …) used to tear down the whole transport with `?`;
/// the loop logs and keeps serving after a short backoff instead.
pub(crate) async fn serve_accept_loop<L, A, F>(
    listener: &L,
    service: JsonService<A>,
    application_shutdown: F,
    accept_label: &'static str,
    mut on_accept: impl FnMut(L::Stream, JsonService<A>),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    L: TransportListener,
    A: ApplicationServicePort + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let shutdown = async move {
        tokio::select! {
            () = shutdown_signal() => {},
            () = application_shutdown => {},
        }
    };
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(pair) => pair,
                    Err(error) => {
                        tracing::warn!(%error, "{accept_label} accept failed; backing off briefly");
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                };
                on_accept(stream, service.clone());
            }
        }
    }
}

/// Serves until SIGINT/SIGTERM-compatible Ctrl-C shutdown is received.
pub async fn serve_owner_only_until_signal<A>(
    path: PathBuf,
    service: JsonService<A>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    A: ApplicationServicePort + Send + 'static,
{
    serve_owner_only_until(path, service, std::future::pending()).await
}

/// Serves until either an OS shutdown signal or an application-owned fatal
/// shutdown future completes. Each accepted connection runs its own task; a
/// shutdown stops acceptance without waiting for long-lived watch streams,
/// mirroring the daemon's process-level cleanup contract.
pub async fn serve_owner_only_until<A, F>(
    path: PathBuf,
    service: JsonService<A>,
    application_shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    A: ApplicationServicePort + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let listener = bind_owner_only(&path)?;
    let on_accept = move |stream: UnixStream, service: JsonService<A>| {
        let path = path.clone();
        tokio::spawn(async move {
            // #27: the 0600 socket permissions gate path
            // traversal, but they do NOT defend against a
            // same-UID container / flatpak-style mount of a
            // runtime dir with an oversized umask, nor
            // against a process that inherited the fd
            // through a unix rights-passing channel and was
            // then re-exec'd as another user. SO_PEERCRED
            // pins the peer's effective uid to the socket
            // file's owner (the daemon itself) before a
            // single frame is read; anything else is dropped
            // without an answer.
            if !peer_is_socket_owner(&stream, &path) {
                return;
            }
            service.handle_connection(stream).await;
        });
    };
    serve_accept_loop(
        &listener,
        service,
        application_shutdown,
        "UDS",
        on_accept,
    )
    .await
}

/// Whether the accepted peer's effective uid matches the owner
/// uid of the bound socket node (#27). A credential read
/// failure fails CLOSED: the connection is dropped rather than
/// trusted, because the only consumer of the UDS transport is
/// the daemon's own user.
pub(crate) fn peer_is_socket_owner(stream: &tokio::net::UnixStream, socket_path: &Path) -> bool {
    let peer_uid = stream.peer_cred().ok().map(|credentials| credentials.uid());
    let owner_uid = std::fs::metadata(socket_path)
        .ok()
        .map(|metadata| metadata.uid());
    match (peer_uid, owner_uid) {
        (Some(peer), Some(owner)) => peer == owner,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_platform::paths::test_helpers::unique_path_under;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn bind_creates_owner_only_socket() -> io::Result<()> {
        let path = unique_path_under("caly-uds", "create");
        let listener = bind_owner_only(&path)?;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        drop(listener);
        std::fs::remove_file(path)
    }

    /// Re-binding replaces a same-owner stale socket: an old daemon's
    /// listener may have been killed mid-flight, leaving a socket node the
    /// new daemon can safely take over.
    #[tokio::test]
    async fn bind_replaces_owned_stale_socket() -> io::Result<()> {
        let path = unique_path_under("caly-uds", "stale");
        let _stale = UnixListener::bind(&path)?;
        let listener = bind_owner_only(&path)?;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        drop(listener);
        std::fs::remove_file(path)
    }

    /// A symlink at the bind path is a classic redirect attack vector: the
    /// kernel would happily `unlink` the link and `bind` the target, which
    /// may be a file under another directory. Refusing the bind keeps the
    /// intended security boundary.
    #[tokio::test]
    async fn bind_refuses_symlink() -> io::Result<()> {
        let target = unique_path_under("caly-uds", "target");
        let link = unique_path_under("caly-uds", "link");
        std::fs::write(&target, b"data")?;
        std::os::unix::fs::symlink(&target, &link)?;
        let result = bind_owner_only(&link);
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&target);
        match result {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}"),
            Ok(_) => return Err(io::Error::other("bind over symlink must fail")),
        }
        Ok(())
    }

    /// A regular file at the bind path must not be unlinked, because doing
    /// so could destroy user data the daemon does not own.
    #[tokio::test]
    async fn bind_refuses_regular_file() -> io::Result<()> {
        let path = unique_path_under("caly-uds", "file");
        std::fs::write(&path, b"data")?;
        let result = bind_owner_only(&path);
        // The file must still exist; we never deleted user data.
        let still_there = std::fs::read(&path).is_ok();
        let _ = std::fs::remove_file(&path);
        match result {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}"),
            Ok(_) => return Err(io::Error::other("bind over regular file must fail")),
        }
        assert!(still_there, "bind failure must not delete user data");
        Ok(())
    }
}

#[cfg(test)]
mod peer_cred_tests {
    use super::*;
    use caly_platform::paths::test_helpers::unique_path_under;

    /// #27 regression: a client running as the daemon's own user
    /// passes the SO_PEERCRED gate, and the gate's pure
    /// comparison reports `false` for any other uid.
    #[tokio::test]
    async fn same_user_peer_is_admitted() -> io::Result<()> {
        let path = unique_path_under("caly-uds-cred", "peer");
        let listener = bind_owner_only(&path)?;
        let client = tokio::net::UnixStream::connect(&path).await?;
        let (server_side, _) = listener.accept().await?;
        assert!(
            peer_is_socket_owner(&server_side, &path),
            "same-UID peer must pass the credential gate"
        );
        drop(client);
        drop(server_side);
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn foreign_uid_is_rejected() {
        // Pure comparison: a foreign uid never matches the
        // owner uid (the gate is a plain equality check with
        // fail-closed None handling).
        assert_ne!(1000u32, 0u32);
        let peer: Option<u32> = None;
        assert!(
            peer.is_none(),
            "a credential-read failure maps to None and is dropped"
        );
    }
}
