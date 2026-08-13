//! Thin-client protocol boundary.

mod contract;
pub mod uds;

pub use contract::{ClientContract, ClientError};
pub use uds::UdsClient;
