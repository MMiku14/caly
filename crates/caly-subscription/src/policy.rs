//! Subscription fetch security and resource policy.

use std::{net::IpAddr, time::Duration};

use caly_domain::BoundedVec;

/// Maximum DNS answers pinned for one fetch.
pub const MAX_DNS_ANSWERS: usize = 32;
pub type ResolvedAddresses = BoundedVec<IpAddr, MAX_DNS_ANSWERS>;

/// Maximum redirect hops followed when `redirects_allowed` is enabled.
pub const MAX_REDIRECT_DEPTH: usize = 3;

/// Only HTTP(S) subscription schemes are accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionScheme {
    Http,
    Https,
}

/// Fail-closed fetch policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FetchPolicy {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_body_bytes: usize,
    pub redirects_allowed: bool,
    pub use_environment_proxy: bool,
}

impl FetchPolicy {
    /// Safe direct-fetch defaults.
    pub const fn direct_default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            max_body_bytes: 32 * 1_024 * 1_024,
            redirects_allowed: false,
            use_environment_proxy: false,
        }
    }
}

/// Classifier supplied by the networking infrastructure and tested independently.
pub trait AddressClassifier {
    fn is_globally_routable(&self, address: IpAddr) -> bool;
}

/// Resolution rejection before a connection is opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionError {
    NoAddresses,
    NonPublicAddress(IpAddr),
}

/// Requires every answer to be public; caller must pin the accepted set.
pub fn validate_resolved(
    addresses: ResolvedAddresses,
    classifier: &impl AddressClassifier,
) -> Result<ResolvedAddresses, ResolutionError> {
    if addresses.is_empty() {
        return Err(ResolutionError::NoAddresses);
    }
    for address in &addresses {
        if !classifier.is_globally_routable(*address) {
            return Err(ResolutionError::NonPublicAddress(*address));
        }
    }
    Ok(addresses)
}
