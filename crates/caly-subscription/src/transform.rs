//! Subscription transform fingerprint independent of HTTP body validators.

use caly_domain::{BoundedText, BoundedVec};
use sha2::{Digest, Sha256};

pub const MAX_TRANSFORM_RULES: usize = 256;
pub type TransformRule = BoundedText<1_024>;
pub type TransformRules = BoundedVec<TransformRule, MAX_TRANSFORM_RULES>;

/// Inputs that can change normalized nodes without changing response bytes.
pub struct TransformInputs {
    pub include: TransformRules,
    pub exclude: TransformRules,
    pub add_tags: TransformRules,
    pub group_rules: TransformRules,
}

/// Stable SHA-256 transform identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransformFingerprint(pub [u8; 32]);

pub fn fingerprint(inputs: &TransformInputs) -> TransformFingerprint {
    let mut hash = Sha256::new();
    hash.update(b"caly-subscription-transform-v1");
    encode_rules(&mut hash, b"include", &inputs.include);
    encode_rules(&mut hash, b"exclude", &inputs.exclude);
    encode_rules(&mut hash, b"tags", &inputs.add_tags);
    encode_rules(&mut hash, b"groups", &inputs.group_rules);
    TransformFingerprint(hash.finalize().into())
}

/// A 304/body hash hit still re-runs transforms when transform identity changed.
pub fn should_retransform(
    cached_body_hash: [u8; 32],
    current_body_hash: [u8; 32],
    cached_transform: TransformFingerprint,
    current_transform: TransformFingerprint,
) -> bool {
    cached_body_hash != current_body_hash || cached_transform.0 != current_transform.0
}

fn encode_rules(hash: &mut Sha256, label: &[u8], rules: &TransformRules) {
    hash.update((label.len() as u64).to_be_bytes());
    hash.update(label);
    hash.update((rules.len() as u64).to_be_bytes());
    for rule in rules {
        hash.update((rule.len_bytes() as u64).to_be_bytes());
        hash.update(rule.as_str().as_bytes());
    }
}
