//! HTTPS fetch with validated DNS pinning and bounded response streaming.

use std::net::SocketAddr;

use caly_domain::BoundedText;
use reqwest::{StatusCode, header};
use url::Url;

use super::{
    AddressClassifier, FetchPolicy, ResolvedAddresses, SubscriptionBody, SubscriptionUserInfo,
    parse_subscription_userinfo, validate_resolved,
};

/// Conditional request metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FetchValidators {
    pub etag: Option<BoundedText<1_024>>,
    pub last_modified: Option<BoundedText<1_024>>,
}

/// Successful bounded fetch result.
pub enum FetchResult {
    NotModified,
    Updated {
        body: SubscriptionBody,
        etag: Option<BoundedText<1_024>>,
        last_modified: Option<BoundedText<1_024>>,
        /// Optional subscription quota metadata from the `subscription-userinfo`
        /// header (upload/download/total/expire).
        userinfo: SubscriptionUserInfo,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum FetchError {
    InvalidUrl,
    UnsupportedScheme,
    MissingHost,
    MissingPort,
    ResolutionRejected,
    ClientBuild,
    /// Underlying transport failure; carries the HTTP client error detail so
    /// the real reason (DNS, connect, TLS, timeout, body read) is surfaced.
    RequestFailed(String),
    UnexpectedStatus(u16),
    BodyTooLarge,
    HeaderTooLong,
}

impl core::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidUrl => formatter.write_str("subscription URL is invalid"),
            Self::UnsupportedScheme => formatter.write_str("URL scheme must be http or https"),
            Self::MissingHost => formatter.write_str("subscription URL has no host"),
            Self::MissingPort => formatter.write_str("subscription URL has no port"),
            Self::ResolutionRejected => {
                formatter.write_str("subscription host resolved only to non-public addresses")
            }
            Self::ClientBuild => formatter.write_str("HTTP client construction failed"),
            Self::RequestFailed(reason) => {
                write!(formatter, "HTTP request failed: {reason}")
            }
            Self::UnexpectedStatus(status) => {
                write!(formatter, "subscription source returned HTTP {status}")
            }
            Self::BodyTooLarge => formatter.write_str("subscription body is too large"),
            Self::HeaderTooLong => formatter.write_str("subscription header is too long"),
        }
    }
}

/// Fetches with redirects/environment proxy disabled and DNS answers pinned.
pub async fn fetch_pinned(
    source: &str,
    addresses: ResolvedAddresses,
    classifier: &impl AddressClassifier,
    policy: FetchPolicy,
    validators: &FetchValidators,
) -> Result<FetchResult, FetchError> {
    let url = Url::parse(source).map_err(|_| FetchError::InvalidUrl)?;
    // W2-β2a (CLI v3 Q5): `sub add` accepts local files; they are
    // stored as canonical `file://` URLs and land here. A file has
    // no validators (`304 Not-Modified` does not exist on disk), so
    // every refresh re-reads the whole body; the byte count is
    // bounded from metadata first, then re-checked after the read
    // (TOCTOU double-gate, same limit as the HTTP path).
    if url.scheme() == "file" {
        return read_file_body(&url, policy);
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::UnsupportedScheme);
    }
    let host = url.host_str().ok_or(FetchError::MissingHost)?;
    let port = url.port_or_known_default().ok_or(FetchError::MissingPort)?;
    let addresses =
        validate_resolved(addresses, classifier).map_err(|_| FetchError::ResolutionRejected)?;
    let sockets: Vec<_> = addresses
        .iter()
        .map(|address| SocketAddr::new(*address, port))
        .collect();
    let client = build_client(host, &sockets, policy)?;
    let mut response = send_request(&client, url, policy, validators).await?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(FetchResult::NotModified);
    }
    if !response.status().is_success() {
        return Err(FetchError::UnexpectedStatus(response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > policy.max_body_bytes as u64)
    {
        return Err(FetchError::BodyTooLarge);
    }
    let etag = bounded_header(response.headers(), header::ETAG)?;
    let last_modified = bounded_header(response.headers(), header::LAST_MODIFIED)?;
    // Optional quota metadata; an absent/malformed header yields all-None.
    let userinfo = response
        .headers()
        .get("subscription-userinfo")
        .and_then(|value| value.to_str().ok())
        .map_or_else(
            || SubscriptionUserInfo {
                upload_bytes: None,
                download_bytes: None,
                total_bytes: None,
                expire_unix: None,
            },
            parse_subscription_userinfo,
        );
    let body = read_bounded_body(&mut response, policy.max_body_bytes).await?;
    Ok(FetchResult::Updated {
        body,
        etag,
        last_modified,
        userinfo,
    })
}

async fn send_request(
    client: &reqwest::Client,
    url: Url,
    policy: FetchPolicy,
    validators: &FetchValidators,
) -> Result<reqwest::Response, FetchError> {
    let mut request = client.get(url).timeout(policy.request_timeout);
    if let Some(etag) = &validators.etag {
        request = request.header(header::IF_NONE_MATCH, etag.as_str());
    }
    if let Some(modified) = &validators.last_modified {
        request = request.header(header::IF_MODIFIED_SINCE, modified.as_str());
    }
    request
        .send()
        .await
        .map_err(|error| FetchError::RequestFailed(error.to_string()))
}

fn build_client(
    host: &str,
    sockets: &[SocketAddr],
    policy: FetchPolicy,
) -> Result<reqwest::Client, FetchError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(policy.connect_timeout)
        .resolve_to_addrs(host, sockets);
    // Honor the configured fetch policy instead of hard-coding the safe
    // defaults: redirects and system proxies are only enabled when the user
    // explicitly opts in (both default to false/off).
    if policy.redirects_allowed {
        // Audit #111: even when redirects are enabled they must stay on the
        // *pinned* host — DNS pinning above only covers the original host,
        // and an unrestricted follow to an operator-configured 302 target
        // would defeat the SSRF containment (e.g. bouncing to 169.254.169.254
        // or any internal endpoint). A cross-host hop stops the chain:
        // reqwest surfaces it as a request error.
        let origin = host.to_owned();
        builder = builder.redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let too_deep = attempt.previous().len() >= crate::MAX_REDIRECT_DEPTH;
            let same_host = attempt
                .url()
                .host_str()
                .is_some_and(|target| target.eq_ignore_ascii_case(&origin));
            if too_deep || !same_host {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }));
    } else {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }
    if !policy.use_environment_proxy {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|_| FetchError::ClientBuild)
}

/// W2-β2a: the `file://` arm of [`fetch_pinned`] — read a local
/// subscription document with the same `max_body_bytes` ceiling the
/// HTTP path honours. Every refresh re-reads (no etag cadence on
/// disk); read/stat failures surface as `RequestFailed` so the
/// caller's transient-fetch retry policy treats them like an
/// unreachable peer.
fn read_file_body(url: &Url, policy: FetchPolicy) -> Result<FetchResult, FetchError> {
    let path = url.to_file_path().map_err(|()| FetchError::InvalidUrl)?;
    let metadata = std::fs::metadata(&path).map_err(|error| {
        FetchError::RequestFailed(format!(
            "could not stat local file `{}`: {error}",
            path.display()
        ))
    })?;
    if metadata.len() > policy.max_body_bytes as u64 {
        return Err(FetchError::BodyTooLarge);
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        FetchError::RequestFailed(format!(
            "could not read local file `{}`: {error}",
            path.display()
        ))
    })?;
    Ok(FetchResult::Updated {
        body: SubscriptionBody::try_from_vec(bytes).map_err(|_| FetchError::BodyTooLarge)?,
        etag: None,
        last_modified: None,
        userinfo: SubscriptionUserInfo {
            upload_bytes: None,
            download_bytes: None,
            total_bytes: None,
            expire_unix: None,
        },
    })
}

async fn read_bounded_body(
    response: &mut reqwest::Response,
    limit: usize,
) -> Result<SubscriptionBody, FetchError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| FetchError::RequestFailed(error.to_string()))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(FetchError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    SubscriptionBody::try_from_vec(body).map_err(|_| FetchError::BodyTooLarge)
}

fn bounded_header(
    headers: &header::HeaderMap,
    name: header::HeaderName,
) -> Result<Option<BoundedText<1_024>>, FetchError> {
    headers
        .get(name)
        .map(|value| {
            let value = value.to_str().map_err(|_| FetchError::HeaderTooLong)?;
            BoundedText::new(value.to_owned()).map_err(|_| FetchError::HeaderTooLong)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Minimal HTTP/1.1 server: `/redirect` answers 302 to `/target`, which
    /// answers `200 ok`. Everything else answers 404.
    async fn spawn_redirect_server() -> (String, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buffer = [0_u8; 2048];
                    let Ok(read) = stream.read(&mut buffer).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let (status, body, location) = if request.starts_with("GET /redirect ") {
                        ("302 Found", "", Some("/target".to_owned()))
                    } else if request.starts_with("GET /target ") {
                        ("200 OK", "ok", None)
                    } else {
                        ("404 Not Found", "missing", None)
                    };
                    let mut response =
                        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
                    if let Some(location) = location {
                        use std::fmt::Write as _;
                        let _ = write!(response, "Location: {location}\r\n");
                    }
                    response.push_str("\r\n");
                    response.push_str(body);
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        ("localhost".to_owned(), address)
    }

    async fn fetch_with_policy(policy: FetchPolicy) -> Result<String, FetchError> {
        let (host, address) = spawn_redirect_server().await;
        let client = build_client(
            &host,
            &[SocketAddr::new(address.ip(), address.port())],
            policy,
        )?;
        let response = client
            .get(format!("http://{host}:{}/redirect", address.port()))
            .send()
            .await
            .map_err(|error| FetchError::RequestFailed(error.to_string()))?;
        if !response.status().is_success() {
            return Err(FetchError::UnexpectedStatus(response.status().as_u16()));
        }
        response
            .text()
            .await
            .map_err(|error| FetchError::RequestFailed(error.to_string()))
    }

    #[tokio::test]
    async fn redirects_are_not_followed_by_default() {
        let result = fetch_with_policy(FetchPolicy::direct_default()).await;
        assert_eq!(result, Err(FetchError::UnexpectedStatus(302)));
    }

    #[tokio::test]
    async fn redirects_are_followed_when_allowed() -> Result<(), String> {
        let policy = FetchPolicy {
            redirects_allowed: true,
            ..FetchPolicy::direct_default()
        };
        let body = fetch_with_policy(policy)
            .await
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(body, "ok");
        Ok(())
    }

    /// Test-local classifier: the `file://` arm never consults it,
    /// so any answer is fine.
    struct AcceptAll;

    impl AddressClassifier for AcceptAll {
        fn is_globally_routable(&self, _address: std::net::IpAddr) -> bool {
            true
        }
    }

    fn unique_temp_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "caly-sub-http-{tag}-{pid}",
            pid = std::process::id()
        ))
    }

    #[tokio::test]
    async fn file_scheme_reads_the_local_body() {
        let path = unique_temp_file("read");
        std::fs::write(&path, b"proxies: []\n").unwrap();
        let url = Url::from_file_path(&path).unwrap();
        let result = fetch_pinned(
            url.as_str(),
            ResolvedAddresses::try_from_vec(Vec::new()).unwrap(),
            &AcceptAll,
            FetchPolicy::direct_default(),
            &FetchValidators::default(),
        )
        .await;
        let _ = std::fs::remove_file(&path);
        let body = match result {
            Ok(FetchResult::Updated { body, etag, .. }) => {
                assert!(etag.is_none(), "file sources carry no validators");
                body
            }
            Ok(FetchResult::NotModified) => panic!("file sources never answer 304"),
            Err(error) => panic!("expected an Updated body, got {error}"),
        };
        assert_eq!(body.as_slice(), b"proxies: []\n");
    }

    #[tokio::test]
    async fn file_scheme_honours_the_body_ceiling() {
        let path = unique_temp_file("ceiling");
        std::fs::write(&path, vec![b'x'; 64]).unwrap();
        let url = Url::from_file_path(&path).unwrap();
        let policy = FetchPolicy {
            max_body_bytes: 8,
            ..FetchPolicy::direct_default()
        };
        let result = fetch_pinned(
            url.as_str(),
            ResolvedAddresses::try_from_vec(Vec::new()).unwrap(),
            &AcceptAll,
            policy,
            &FetchValidators::default(),
        )
        .await;
        let _ = std::fs::remove_file(&path);
        assert!(matches!(result, Err(FetchError::BodyTooLarge)));
    }

    #[tokio::test]
    async fn file_scheme_missing_file_is_a_request_failure() {
        let path = unique_temp_file("missing");
        let url = Url::from_file_path(&path).unwrap();
        let result = fetch_pinned(
            url.as_str(),
            ResolvedAddresses::try_from_vec(Vec::new()).unwrap(),
            &AcceptAll,
            FetchPolicy::direct_default(),
            &FetchValidators::default(),
        )
        .await;
        assert!(matches!(result, Err(FetchError::RequestFailed(_))));
    }
}
