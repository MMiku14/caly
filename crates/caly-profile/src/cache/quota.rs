//! Deterministic cache quota eviction planning.

use caly_domain::BoundedVec;

/// Maximum generations considered in one cache root.
pub const MAX_CACHE_GENERATIONS: usize = 1_024;
pub type EvictionPlan = BoundedVec<[u8; 16], MAX_CACHE_GENERATIONS>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationUsage {
    pub id: [u8; 16],
    pub bytes: u64,
    pub created_sequence: u64,
    pub is_current: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaError {
    TooManyGenerations,
    CurrentAloneExceedsQuota,
    /// Audit #95: evicting every non-current generation still leaves the
    /// cache over quota (byte ceiling, or more current generations than
    /// `max_generations`). The pre-fix shape returned an *incomplete* plan
    /// as `Ok`, letting callers believe the quota had been reached.
    Unsatisfiable,
}

/// Selects oldest non-current generations until both quotas are satisfied.
pub fn plan_evictions(
    mut generations: Vec<GenerationUsage>,
    max_generations: usize,
    max_bytes: u64,
) -> Result<EvictionPlan, QuotaError> {
    if generations.len() > MAX_CACHE_GENERATIONS {
        return Err(QuotaError::TooManyGenerations);
    }
    let current_bytes = generations
        .iter()
        .filter(|value| value.is_current)
        .fold(0_u64, |total, value| total.saturating_add(value.bytes));
    if current_bytes > max_bytes {
        return Err(QuotaError::CurrentAloneExceedsQuota);
    }
    generations.sort_by_key(|value| value.created_sequence);
    let mut retained_count = generations.len();
    let mut retained_bytes = generations
        .iter()
        .fold(0_u64, |total, value| total.saturating_add(value.bytes));
    let mut evictions = Vec::new();
    for generation in generations {
        if retained_count <= max_generations && retained_bytes <= max_bytes {
            break;
        }
        if generation.is_current {
            continue;
        }
        retained_count = retained_count.saturating_sub(1);
        retained_bytes = retained_bytes.saturating_sub(generation.bytes);
        evictions.push(generation.id);
    }
    if retained_count > max_generations || retained_bytes > max_bytes {
        // Audit #95: report the shortfall instead of returning an
        // incomplete plan (e.g. two current generations with
        // `max_generations = 1` can never be reconciled by eviction).
        return Err(QuotaError::Unsatisfiable);
    }
    EvictionPlan::try_from_vec(evictions).map_err(|_| QuotaError::TooManyGenerations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(id: u8, bytes: u64, sequence: u64, current: bool) -> GenerationUsage {
        GenerationUsage {
            id: [id; 16],
            bytes,
            created_sequence: sequence,
            is_current: current,
        }
    }

    #[test]
    fn incomplete_eviction_is_an_error_not_a_silent_plan() {
        // Two current generations with max_generations = 1: nothing can be
        // evicted to satisfy the count quota.
        let plan = plan_evictions(vec![usage(1, 10, 1, true), usage(2, 10, 2, true)], 1, 1_000);
        assert_eq!(plan, Err(QuotaError::Unsatisfiable));
    }

    #[test]
    fn evicting_non_current_reaches_the_byte_quota() -> Result<(), String> {
        // With current_bytes below the ceiling, evicting the old generations
        // always satisfies the byte quota — the only infeasible shape is a
        // current-count overflow (covered above).
        let generations = vec![usage(1, 5, 1, false), usage(2, 2, 2, true)];
        match plan_evictions(generations, 8, 4) {
            Ok(plan) if plan.as_slice() == [[1u8; 16]] => Ok(()),
            other => Err(format!("unexpected plan: {other:?}")),
        }
    }

    #[test]
    fn normal_eviction_still_plans_oldest_first() -> Result<(), String> {
        let generations = vec![
            usage(1, 5, 1, false),
            usage(2, 5, 2, false),
            usage(3, 5, 3, true),
        ];
        match plan_evictions(generations, 2, 100) {
            Ok(plan) if plan.as_slice() == [[1u8; 16]] => Ok(()),
            other => Err(format!("unexpected plan: {other:?}")),
        }
    }
}
