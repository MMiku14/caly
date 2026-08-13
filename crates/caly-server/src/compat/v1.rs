//! Explicitly lossy v1 compatibility boundary.

use caly_domain::BoundedVec;

/// Maximum compatibility losses attached to one conversion.
pub const MAX_COMPAT_LOSSES: usize = 16;
/// Bounded compatibility loss list.
pub type CompatLosses = BoundedVec<LossKind, MAX_COMPAT_LOSSES>;

/// Information lost while converting between v1 and v2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LossKind {
    UnknownEnum,
    MissingOperationStatus,
    MissingDaemonEpoch,
    MissingFeatureNegotiation,
}

/// Required observer for every lossy compatibility conversion.
pub trait CompatMetrics {
    fn record_loss(&mut self, kind: LossKind);
}

/// Result that cannot hide compatibility loss.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lossy<T> {
    pub value: T,
    pub losses: CompatLosses,
}

impl<T> Lossy<T> {
    /// Records all losses before returning the converted value.
    pub fn observe(self, metrics: &mut impl CompatMetrics) -> T {
        for loss in &self.losses {
            metrics.record_loss(*loss);
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Metrics(Vec<LossKind>);

    impl CompatMetrics for Metrics {
        fn record_loss(&mut self, kind: LossKind) {
            self.0.push(kind);
        }
    }

    #[test]
    fn lossy_conversion_cannot_hide_metrics() -> Result<(), caly_domain::CapacityError> {
        let losses = CompatLosses::try_from_vec(vec![
            LossKind::MissingOperationStatus,
            LossKind::MissingDaemonEpoch,
        ])?;
        let mut metrics = Metrics::default();
        let value = Lossy {
            value: 7_u8,
            losses,
        }
        .observe(&mut metrics);
        assert_eq!(value, 7);
        assert_eq!(metrics.0.len(), 2);
        Ok(())
    }
}
