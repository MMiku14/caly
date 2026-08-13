//! DNS capability crate: the bounded model (settings, filters, structured
//! nameserver values) plus the reachability probe and its wire codec.
//!
//! Relocated in P4 (docs/crate-replan.md v4.1): the model half came from
//! `caly-domain` (`dns`, `dns_filter`, `nameserver`), the probe/wire half
//! from `caly-platform` (`dns_probe`). The probe's transaction id is
//! injected by the caller (§5.12: deterministic testability, not purity);
//! caly-dns stays free of `caly-platform`, raw UDP sockets are sanctioned
//! for capability crates by M12.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny

mod filter;
mod nameserver;
pub mod probe;
mod settings;

pub use filter::{
    DNS_LISTEN_MAX_BYTES, DNS_PATTERN_MAX_BYTES, DnsFilterError, DnsPattern, FallbackFilter,
    MAX_DNS_FILTER_ENTRIES,
};
pub use nameserver::{
    DNS_TEXT_MAX_BYTES, FAKE_IP_RANGE_MAX_BYTES, FakeIpRange, FakeIpRangeError, Nameserver,
    NameserverError, NameserverKind, NameserverShape,
};
pub use settings::{
    DnsError, DnsMode, DnsSettings, DnsSettingsBuilder, MAX_DNS_SERVERS, default_tun_dns,
};
