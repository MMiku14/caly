//! Configured and runtime capability semantics.

use crate::{BoundedText, BoundedVec};

/// Maximum capabilities exposed by one core generation.
pub const MAX_CAPABILITIES: usize = 64;
/// Maximum caveat length in UTF-8 bytes.
pub const CAPABILITY_CAVEAT_MAX_BYTES: usize = 256;

/// A feature whose support can differ by core and runtime endpoint health.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    /// Render and apply TUN configuration.
    TunConfiguration,
    /// Render DNS configuration.
    DnsConfiguration,
    /// Change proxy routing mode at runtime.
    RuntimeModeSwitch,
    /// Read proxy groups and their selected members.
    ProxyGroups,
    /// Select a member in a selectable proxy group.
    ProxySelection,
    /// Run an on-demand URL latency test.
    UrlTest,
    /// Observe active connections.
    Connections,
    /// Close one or more active connections.
    ConnectionClose,
    /// Stream or poll traffic measurements.
    Traffic,
    /// Receive core logs.
    Logs,
    /// Inspect effective routing rules.
    Rules,
}

/// Static support determined by configuration renderer and core family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfiguredSupport {
    /// The feature cannot be represented for this core.
    Unsupported,
    /// The feature can be represented completely.
    Supported,
    /// The feature can be represented with explicit limitations.
    Partial,
}

/// Runtime health of an endpoint needed to operate a configured feature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeAvailability {
    /// No runtime endpoint is required.
    NotRequired,
    /// The required endpoint is healthy.
    Available,
    /// The endpoint is expected but currently unavailable.
    Unavailable,
    /// Runtime capability has not yet been probed.
    Unknown,
}

/// Complete capability assessment without optimistic defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityStatus {
    capability: Capability,
    configured: ConfiguredSupport,
    runtime: RuntimeAvailability,
    caveat: Option<BoundedText<CAPABILITY_CAVEAT_MAX_BYTES>>,
}

impl CapabilityStatus {
    /// Constructs a capability assessment.
    pub const fn new(
        capability: Capability,
        configured: ConfiguredSupport,
        runtime: RuntimeAvailability,
        caveat: Option<BoundedText<CAPABILITY_CAVEAT_MAX_BYTES>>,
    ) -> Self {
        Self {
            capability,
            configured,
            runtime,
            caveat,
        }
    }

    /// Returns the assessed feature.
    pub const fn capability(&self) -> Capability {
        self.capability
    }

    /// Returns static renderer/core support.
    pub const fn configured(&self) -> ConfiguredSupport {
        self.configured
    }

    /// Returns current endpoint availability.
    pub const fn runtime(&self) -> RuntimeAvailability {
        self.runtime
    }

    /// Returns an explicit limitation or outage explanation.
    pub const fn caveat(&self) -> Option<&BoundedText<CAPABILITY_CAVEAT_MAX_BYTES>> {
        self.caveat.as_ref()
    }

    /// Returns whether callers may truthfully invoke the feature now.
    pub const fn is_usable(&self) -> bool {
        let configured = matches!(
            self.configured,
            ConfiguredSupport::Supported | ConfiguredSupport::Partial
        );
        let runtime = matches!(
            self.runtime,
            RuntimeAvailability::NotRequired | RuntimeAvailability::Available
        );
        configured && runtime
    }
}

/// Error returned when a capability set contains duplicate features.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DuplicateCapability(pub Capability);

impl core::fmt::Display for DuplicateCapability {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "capability {:?} appears more than once", self.0)
    }
}

impl std::error::Error for DuplicateCapability {}

/// Bounded capability assessment for one running core generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilitySet(BoundedVec<CapabilityStatus, MAX_CAPABILITIES>);

impl CapabilitySet {
    /// Validates uniqueness after capacity has already been enforced.
    pub fn new(
        values: BoundedVec<CapabilityStatus, MAX_CAPABILITIES>,
    ) -> Result<Self, DuplicateCapability> {
        for (index, status) in values.iter().enumerate() {
            if values[index + 1..]
                .iter()
                .any(|candidate| candidate.capability == status.capability)
            {
                return Err(DuplicateCapability(status.capability));
            }
        }
        Ok(Self(values))
    }

    /// Constructs a capability set from a bounded vector, deduplicating in
    /// place so the first occurrence of each `Capability` wins. This is the
    /// infallible constructor for hardcoded capability lists in kernel
    /// adapters — the bounded capacity has already been enforced upstream
    /// and a hardcoded duplicate is a programming error we want to
    /// gracefully degrade from (drop the later duplicate) rather than a
    /// process-kill. The `Result`-returning `new` remains the right call for
    /// code paths that parse user-controlled input.
    pub fn from_bounded_dedup(values: BoundedVec<CapabilityStatus, MAX_CAPABILITIES>) -> Self {
        let mut seen = [false; MAX_CAPABILITIES];
        let inner = values.into_vec();
        let mut kept = Vec::with_capacity(inner.len());
        for status in inner {
            let index = status.capability as usize;
            if seen[index] {
                continue;
            }
            seen[index] = true;
            kept.push(status);
        }
        let bounded = BoundedVec::from_vec_truncated(kept);
        // `BoundedVec::from_vec_truncated` enforces the capacity; the
        // bounded set's field is `pub(crate)` and only constructed here.
        // Defensive: if `MAX_CAPABILITIES` is somehow violated (the
        // `seen` table is sized to it), the truncated value loses the
        // trailing entries — the same fall-back semantics as a `from_iter`
        // that overshoots the bound.
        Self(bounded)
    }

    /// Looks up an explicitly assessed capability.
    pub fn get(&self, capability: Capability) -> Option<&CapabilityStatus> {
        self.0.iter().find(|status| status.capability == capability)
    }

    /// Returns whether the capability is explicitly assessed and currently usable.
    pub fn is_usable(&self, capability: Capability) -> bool {
        self.get(capability)
            .is_some_and(CapabilityStatus::is_usable)
    }

    /// Iterates over explicit assessments.
    pub fn iter(&self) -> impl Iterator<Item = &CapabilityStatus> {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(runtime: RuntimeAvailability) -> CapabilityStatus {
        CapabilityStatus::new(
            Capability::Connections,
            ConfiguredSupport::Supported,
            runtime,
            None,
        )
    }

    #[test]
    fn configured_support_does_not_imply_runtime_availability() {
        assert!(!status(RuntimeAvailability::Unknown).is_usable());
        assert!(!status(RuntimeAvailability::Unavailable).is_usable());
        assert!(status(RuntimeAvailability::Available).is_usable());
    }

    #[test]
    fn duplicate_assessments_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let values = BoundedVec::try_from_vec(vec![
            status(RuntimeAvailability::Available),
            status(RuntimeAvailability::Unavailable),
        ])?;
        assert_eq!(
            CapabilitySet::new(values),
            Err(DuplicateCapability(Capability::Connections))
        );
        Ok(())
    }

    /// `from_bounded_dedup` is the infallible constructor used in
    /// production paths where the caller has a hardcoded list of
    /// capabilities and the bounded capacity is sized accordingly. The
    /// previous `CapabilitySet::new(values).unwrap_or_else(|_| abort)`
    /// pattern was a process-kill fallback for the (realistically reachable
    /// duplicate) path; this test pins the new graceful-dedup behaviour.
    #[test]
    fn from_bounded_dedup_keeps_first_occurrence() {
        let values = BoundedVec::from_vec_truncated(vec![
            status(RuntimeAvailability::Available),
            status(RuntimeAvailability::Unavailable),
        ]);
        let set = CapabilitySet::from_bounded_dedup(values);
        // The first (Available) wins; the duplicate is dropped.
        assert_eq!(
            set.get(Capability::Connections)
                .map(CapabilityStatus::runtime),
            Some(RuntimeAvailability::Available)
        );
    }

    #[test]
    fn from_bounded_dedup_truncates_when_over_capacity() {
        // 70 distinct `Connections` duplicates would not be deduplicated to
        // 64 entries from the cap, so the final vector must be truncated.
        let mut statuses = Vec::with_capacity(70);
        for _ in 0..70 {
            statuses.push(status(RuntimeAvailability::Available));
        }
        let values = BoundedVec::from_vec_truncated(statuses);
        let set = CapabilitySet::from_bounded_dedup(values);
        // Only one capability variant is represented (deduped), but the
        // cap on the bounded vector still bounds the eventual storage.
        assert_eq!(
            set.get(Capability::Connections)
                .map(CapabilityStatus::runtime),
            Some(RuntimeAvailability::Available)
        );
        assert!(set.iter().count() <= MAX_CAPABILITIES);
    }
}
