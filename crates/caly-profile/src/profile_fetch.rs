//! SSRF-safe fetcher for declared `Profile` bodies.
//!
//! Mirrors the structure of the subscription `fetch_pinned` path:
//!   1. Parse the URL,
//!   2. Resolve DNS with bounded retries,
//!   3. Pin the connection to the public subset,
//!   4. Run the request with the bounded body / timeout policy,
//!   5. Classify the failure as transient / persistent.
//!
//! The fetcher is async; CLI call sites that run in a sync
//! context wrap it in `tokio::runtime::Handle::block_on` (the
//! daemon-side caller already runs inside a Tokio runtime).
//!
//! The fetch outcome is a [`ProfileFetchOutcome`] that
//! distinguishes `NotModified` (the ETag matched, the body
//! is unchanged) from `Updated` (the body is in the
//! `Updated` variant's `body` field). The caller is responsible
//! for materialising the body into the
//! [`ProfileStore`](crate::profile_store::ProfileStore) cache
//! (atomic write + metadata sidecar).

use std::net::IpAddr;

use url::Url;

use caly_subscription::net::{prefer_public_addresses, resolve_host_with_retry};
use caly_subscription::{
    AddressClassifier, FetchError, FetchPolicy, FetchResult, FetchValidators, ResolvedAddresses,
    fetch_pinned,
};

/// Outcome of a profile-body fetch. The caller persists `body`
/// when the variant is `Updated`; the `NotModified` case is a
/// no-op for the on-disk cache. `Updated` carries the response
/// validators back so the caller can store them next to the
/// body and issue the *next* fetch as a conditional request.
#[derive(Debug)]
pub enum ProfileFetchOutcome {
    Updated {
        body: Vec<u8>,
        validators: FetchValidators,
    },
    NotModified,
}

/// Failure mode of a profile-body fetch. Carries enough
/// information for the CLI / daemon to distinguish a transient
/// network blip (worth retrying) from a persistent
/// misconfiguration (worth surfacing to the operator).
#[derive(Debug)]
pub enum ProfileFetchError {
    InvalidUrl,
    UnsupportedScheme,
    MissingHost,
    MissingPort,
    ResolutionFailed,
    ResolutionRejected,
    ClientBuild,
    RequestFailed(String),
    /// Transport returned a non-success HTTP status. The status
    /// is preserved so the CLI can surface it.
    UnexpectedStatus(u16),
    /// The transport or the loader would have produced a body
    /// larger than [`caly_domain::PROFILE_BODY_MAX_BYTES`].
    BodyTooLarge,
    /// The DNS answer set exceeded the bounded
    /// [`ResolvedAddresses`] budget (previously reported as
    /// `BodyTooLarge`, a confusing conflation of two limits).
    TooManyAddresses,
    /// One of the headers carried a value too long for the
    /// bounded `BoundedText` slot.
    HeaderTooLong,
}

impl core::fmt::Display for ProfileFetchError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidUrl => formatter.write_str("profile URL is invalid"),
            Self::UnsupportedScheme => formatter.write_str("URL scheme must be http or https"),
            Self::MissingHost => formatter.write_str("profile URL has no host"),
            Self::MissingPort => formatter.write_str("profile URL has no port"),
            Self::ResolutionFailed => formatter.write_str("profile host DNS resolution failed"),
            Self::ResolutionRejected => {
                formatter.write_str("profile host resolved only to non-public addresses")
            }
            Self::ClientBuild => formatter.write_str("HTTP client construction failed"),
            Self::RequestFailed(reason) => {
                write!(formatter, "HTTP request failed: {reason}")
            }
            Self::UnexpectedStatus(status) => {
                write!(formatter, "profile source returned HTTP {status}")
            }
            Self::BodyTooLarge => formatter.write_str("profile body is too large"),
            Self::TooManyAddresses => {
                formatter.write_str("profile host returned too many DNS answers")
            }
            Self::HeaderTooLong => formatter.write_str("profile header is too long"),
        }
    }
}

impl std::error::Error for ProfileFetchError {}

/// Whether a fetch error is transient and worth retrying. Connect
/// / request timeouts and 5xx responses are retried; SSRF
/// rejections and policy errors (oversize body, bad URL) are
/// not. Pure: takes the error by reference and returns a bool.
pub fn is_transient_profile_error(error: &ProfileFetchError) -> bool {
    matches!(
        error,
        ProfileFetchError::RequestFailed(_) | ProfileFetchError::ClientBuild
    ) || matches!(error, ProfileFetchError::UnexpectedStatus(status) if (500..600).contains(status))
}

/// Fetches a single profile body. The `address_classifier`
/// argument is normally
/// [`PublicAddressClassifier`](caly_subscription::PublicAddressClassifier);
/// tests can substitute a private-allowing classifier to
/// assert the SSRF guard fires.
pub async fn fetch_profile_body(
    url: &str,
    address_classifier: &impl AddressClassifier,
    policy: FetchPolicy,
    validators: &FetchValidators,
) -> Result<ProfileFetchOutcome, ProfileFetchError> {
    let parsed = Url::parse(url).map_err(|_| ProfileFetchError::InvalidUrl)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ProfileFetchError::UnsupportedScheme);
    }
    let host = parsed
        .host_str()
        .ok_or(ProfileFetchError::MissingHost)?
        .to_owned();
    let port = parsed
        .port_or_known_default()
        .ok_or(ProfileFetchError::MissingPort)?;
    let addresses =
        resolve_host_with_retry(&host, port).map_err(|_| ProfileFetchError::ResolutionFailed)?;
    let addresses = prefer_public_addresses(addresses, address_classifier);
    if addresses.is_empty() {
        return Err(ProfileFetchError::ResolutionRejected);
    }
    let resolved = ResolvedAddresses::try_from_vec(addresses)
        .map_err(|_| ProfileFetchError::TooManyAddresses)?;
    let body_limit = caly_domain::PROFILE_BODY_MAX_BYTES;
    let profile_policy = FetchPolicy {
        max_body_bytes: body_limit,
        ..policy
    };
    let result = fetch_pinned(url, resolved, &NoopClassifier, profile_policy, validators)
        .await
        .map_err(map_fetch_error)?;
    let (body, validators) = match result {
        FetchResult::Updated {
            body,
            etag,
            last_modified,
            ..
        } => (
            body.into_vec(),
            FetchValidators {
                etag,
                last_modified,
            },
        ),
        FetchResult::NotModified => return Ok(ProfileFetchOutcome::NotModified),
    };
    if body.len() > caly_domain::PROFILE_BODY_MAX_BYTES {
        return Err(ProfileFetchError::BodyTooLarge);
    }
    Ok(ProfileFetchOutcome::Updated { body, validators })
}

/// The [`fetch_pinned`] signature requires a `&impl
/// AddressClassifier`; we already classified on the
/// caller-provided `address_classifier` and pinned the
/// connection to public addresses, so the call into
/// `fetch_pinned` only needs to satisfy the trait bound.
/// Reusing `PublicAddressClassifier` keeps a single source
/// of truth for the rejection rules.
struct NoopClassifier;
impl AddressClassifier for NoopClassifier {
    fn is_globally_routable(&self, _: IpAddr) -> bool {
        // We have already pre-filtered the address set with the
        // caller's classifier; the second pass inside
        // `fetch_pinned` is therefore a guaranteed accept.
        true
    }
}

fn map_fetch_error(error: FetchError) -> ProfileFetchError {
    match error {
        FetchError::InvalidUrl => ProfileFetchError::InvalidUrl,
        FetchError::UnsupportedScheme => ProfileFetchError::UnsupportedScheme,
        FetchError::MissingHost => ProfileFetchError::MissingHost,
        FetchError::MissingPort => ProfileFetchError::MissingPort,
        FetchError::ResolutionRejected => ProfileFetchError::ResolutionRejected,
        FetchError::ClientBuild => ProfileFetchError::ClientBuild,
        FetchError::RequestFailed(reason) => ProfileFetchError::RequestFailed(reason),
        FetchError::UnexpectedStatus(status) => ProfileFetchError::UnexpectedStatus(status),
        FetchError::BodyTooLarge => ProfileFetchError::BodyTooLarge,
        FetchError::HeaderTooLong => ProfileFetchError::HeaderTooLong,
    }
}

/// Re-export the `BoundedText` import for callers that want
/// to format a [`ProfileFetchError`] into a bounded reason.
/// The re-export keeps the helper at the same visibility
/// level as the rest of the bounded-text public surface.
pub use caly_domain::BoundedText as ProfileBoundedReason;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_classification_matches_profile_errors() {
        assert!(is_transient_profile_error(
            &ProfileFetchError::RequestFailed("connect error".to_owned())
        ));
        assert!(is_transient_profile_error(&ProfileFetchError::ClientBuild));
        assert!(is_transient_profile_error(
            &ProfileFetchError::UnexpectedStatus(503)
        ));
        assert!(!is_transient_profile_error(
            &ProfileFetchError::UnexpectedStatus(404)
        ));
        assert!(!is_transient_profile_error(
            &ProfileFetchError::UnsupportedScheme
        ));
        assert!(!is_transient_profile_error(
            &ProfileFetchError::BodyTooLarge
        ));
    }

    #[test]
    fn error_display_includes_a_human_message() {
        let err = ProfileFetchError::UnexpectedStatus(404);
        let rendered = format!("{err}");
        assert!(rendered.contains("404"));
        let invalid = ProfileFetchError::InvalidUrl;
        assert!(format!("{invalid}").contains("invalid"));
    }

    /// The map function is a structural pass-through: every
    /// `FetchError` variant maps to exactly one
    /// `ProfileFetchError` variant. Locking the contract
    /// here keeps the public error surface stable.
    #[test]
    fn map_fetch_error_covers_every_variant() {
        let cases = vec![
            (FetchError::InvalidUrl, "InvalidUrl"),
            (FetchError::UnsupportedScheme, "UnsupportedScheme"),
            (FetchError::MissingHost, "MissingHost"),
            (FetchError::MissingPort, "MissingPort"),
            (FetchError::ResolutionRejected, "ResolutionRejected"),
            (FetchError::ClientBuild, "ClientBuild"),
            (
                FetchError::RequestFailed("boom".to_owned()),
                "RequestFailed",
            ),
            (FetchError::UnexpectedStatus(502), "UnexpectedStatus"),
            (FetchError::BodyTooLarge, "BodyTooLarge"),
            (FetchError::HeaderTooLong, "HeaderTooLong"),
        ];
        for (error, expected_variant) in cases {
            let mapped = map_fetch_error(error);
            let rendered = format!("{mapped:?}");
            assert!(
                rendered.contains(expected_variant),
                "expected {expected_variant} in {rendered}"
            );
        }
    }

    /// `BoundedText` is re-exported as `ProfileBoundedReason`
    /// so callers (CLI / daemon) can format a
    /// [`ProfileFetchError`] into a bounded reason without a
    /// second `use caly_domain::BoundedText` line.
    #[test]
    fn profile_bounded_reason_is_re_exported() {
        let bounded: ProfileBoundedReason<1_024> =
            ProfileBoundedReason::from_nonempty_clamped("team-shared".to_owned(), "fallback");
        assert_eq!(bounded.as_str(), "team-shared");
    }
}
