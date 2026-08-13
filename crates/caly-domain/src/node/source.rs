//! Credential-free node provenance.

use crate::{BoundedText, SubscriptionId};

/// Maximum safe import-origin label length.
pub const IMPORT_ORIGIN_MAX_BYTES: usize = 128;

/// Node provenance without subscription URLs or credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeSource {
    /// Produced by a subscription identified outside the node record.
    Subscription(SubscriptionId),
    /// Entered directly by the user.
    Manual,
    /// Imported from a safe, bounded origin label.
    Import(BoundedText<IMPORT_ORIGIN_MAX_BYTES>),
}
