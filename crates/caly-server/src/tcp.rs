//! Loopback TCP transport for the configured `daemon.listen` address.

use std::{future::Future, net::SocketAddr};

use caly_application::service::ApplicationServicePort;
use tokio::net::TcpListener;

use crate::json::JsonService;
use crate::uds::shutdown_signal;

/// Serves the JSON-framed service over a TCP listener until a shutdown signal
/// or the application-owned fatal future completes.
///
/// The TCP transport is the explicitly configured `daemon.listen` endpoint;
/// it shares the same `JsonService` (and thus the daemon session registry)
/// as the owner-only UDS transport.
pub async fn serve_tcp_until<A, F>(
    listen: SocketAddr,
    service: JsonService<A>,
    application_shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    A: ApplicationServicePort + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind(listen).await?;
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
                        // Audit #92: a transient accept error (fd pressure:
                        // EMFILE/ENFILE, a reset racing the accept, …) used
                        // to tear down the whole TCP transport with `?`,
                        // silently degrading the daemon to UDS-only. Log and
                        // keep serving after a short backoff.
                        tracing::warn!(%error, "TCP accept failed; backing off briefly");
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let service = service.clone();
                tokio::spawn(async move {
                    service.handle_connection(stream).await;
                });
            }
        }
    }
}

// ── TLS listener (`daemon.tls_enabled`, #57) ──────────────────

/// Paths to the PEM material for the TLS listener. Parsed once
/// at boot; the daemon refuses to start when
/// `daemon.tls_enabled` is set but either file is unreadable.
#[derive(Clone, Debug)]
pub struct TlsMaterialPaths {
    /// PEM certificate chain presented to clients.
    pub certificate_chain: std::path::PathBuf,
    /// PEM private key (PKCS8 / RSA / EC) for the chain.
    pub private_key: std::path::PathBuf,
}

/// Builds a rustls [`tokio_rustls::TlsAcceptor`] from PEM files
/// on disk. Independent of the serve loop so the parsing failure
/// (wrong key format, mismatched cert) is a boot error, not a
/// per-connection mystery.
pub fn load_tls_acceptor(
    paths: &TlsMaterialPaths,
) -> Result<tokio_rustls::TlsAcceptor, Box<dyn std::error::Error + Send + Sync>> {
    let certificate_bytes = std::fs::read(&paths.certificate_chain).map_err(|error| {
        format!(
            "cannot read TLS certificate chain {}: {error}",
            paths.certificate_chain.display()
        )
    })?;
    let mut certificate_reader = std::io::BufReader::new(certificate_bytes.as_slice());
    let certificates = rustls_pemfile::certs(&mut certificate_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "cannot parse PEM certificates in {}: {error}",
                paths.certificate_chain.display()
            )
        })?;
    if certificates.is_empty() {
        return Err(format!(
            "no PEM certificate found in {}",
            paths.certificate_chain.display()
        )
        .into());
    }

    let key_bytes = std::fs::read(&paths.private_key).map_err(|error| {
        format!(
            "cannot read TLS private key {}: {error}",
            paths.private_key.display()
        )
    })?;
    let mut key_reader = std::io::BufReader::new(key_bytes.as_slice());
    let private_key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|error| {
            format!(
                "cannot parse PEM private key in {}: {error}",
                paths.private_key.display()
            )
        })?
        .ok_or_else(|| {
            format!(
                "no PEM private key found in {}",
                paths.private_key.display()
            )
        })?;

    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| format!("TLS server config rejected the material: {error}"))?;
    Ok(tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config)))
}

/// Serves the JSON-framed service over a TLS-wrapped TCP
/// listener until shutdown. Every accepted connection completes
/// the rustls handshake on its own task before frames flow, so a
/// client that stalls the TLS handshake never blocks the accept
/// loop (and plaintext bytes from a confused client are answered
/// by a rustls alert, not silently probed as JSON).
pub async fn serve_tls_tcp_until<A, F>(
    listen: SocketAddr,
    material: &TlsMaterialPaths,
    service: JsonService<A>,
    application_shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    A: ApplicationServicePort + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let acceptor = load_tls_acceptor(material)?;
    let listener = TcpListener::bind(listen).await?;
    let shutdown = async move {
        tokio::select! {
            () = shutdown_signal() => {},
            () = application_shutdown => {},
        }
    };
    tokio::pin!(shutdown);
    // Audit #92: bound the pre-handshake state a slow-scan/DoS can hold —
    // every accepted connection used to spawn a task whose rustls handshake
    // had no timeout, so a peer holding the TCP connection open without
    // sending a ClientHello could pin tasks (and fds) indefinitely.
    let pre_auth = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_PRE_HANDSHAKE));
    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(pair) => pair,
                    Err(error) => {
                        tracing::warn!(%error, "TLS TCP accept failed; backing off briefly");
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let Ok(permit) = pre_auth.clone().try_acquire_owned() else {
                    tracing::warn!("pre-handshake connection budget exhausted; dropping a new TLS connection");
                    continue;
                };
                let acceptor = acceptor.clone();
                let service = service.clone();
                tokio::spawn(async move {
                    // A failed TLS handshake (plaintext probe,
                    // handshake timeout, wrong protocol) just drops
                    // the connection; the accept loop is unaffected.
                    let handshake =
                        tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream));
                    if let Ok(Ok(tls_stream)) = handshake.await {
                        service.handle_connection(tls_stream).await;
                    }
                    drop(permit);
                });
            }
        }
    }
}

/// Maximum concurrent connections still before/inside the TLS handshake
/// (#92; beyond it new connections are dropped before task spawn).
const MAX_PRE_HANDSHAKE: usize = 128;
/// Overall budget for one TLS handshake (#92; slowloris containment).
const TLS_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
