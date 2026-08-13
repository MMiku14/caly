//! HTTP and cached subscription backends.

mod cached;
mod http;
pub mod render_compose;

pub use cached::CachedSubscriptionBackend;
pub use http::{HttpSubscriptionBackend, subscription_id_for_url};

pub(crate) use caly_subscription::PublicAddressClassifier;

use caly_domain::SubscriptionId;

fn hex_id(id: SubscriptionId) -> String {
    caly_domain::to_hex(id.into_bytes())
}
