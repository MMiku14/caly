//! Clash `subscription-userinfo` header parsing.
//!
//! Real Clash subscription providers return usage metadata in the
//! `subscription-userinfo` response header, e.g.
//! `upload=0; download=0; total=1073741824; expire=1700000000`. This module
//! parses the bounded, optional fields so the CLI can report remaining quota.

/// Parsed subscription usage metadata (all optional, all bounded).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionUserInfo {
    /// Cumulative upload bytes this period.
    pub upload_bytes: Option<u64>,
    /// Cumulative download bytes this period.
    pub download_bytes: Option<u64>,
    /// Total bytes available this period.
    pub total_bytes: Option<u64>,
    /// Unix timestamp (seconds) of quota expiry.
    pub expire_unix: Option<u64>,
}

/// Parses a `subscription-userinfo` header value into usage metadata.
pub fn parse_subscription_userinfo(value: &str) -> SubscriptionUserInfo {
    let mut info = SubscriptionUserInfo {
        upload_bytes: None,
        download_bytes: None,
        total_bytes: None,
        expire_unix: None,
    };
    for part in value.split(';') {
        let part = part.trim();
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();
        match key {
            "upload" => info.upload_bytes = val.parse().ok(),
            "download" => info.download_bytes = val.parse().ok(),
            "total" => info.total_bytes = val.parse().ok(),
            "expire" => info.expire_unix = val.parse().ok(),
            _ => {}
        }
    }
    info
}

/// Renders the usage in human text (or an empty line when nothing is known).
pub fn render_usage_human(info: &SubscriptionUserInfo) -> String {
    match (info.upload_bytes, info.download_bytes, info.total_bytes) {
        (_, _, Some(total)) => {
            let used = info
                .upload_bytes
                .unwrap_or(0)
                .saturating_add(info.download_bytes.unwrap_or(0));
            let remaining = total.saturating_sub(used);
            format!(
                "quota:     {} used / {} total ({} remaining)",
                bytes_human(used),
                bytes_human(total),
                bytes_human(remaining)
            )
        }
        _ => String::new(),
    }
}

/// Formats a byte count in a human-friendly binary unit (integer math, so no
/// `u64`→`f64` precision loss).
fn bytes_human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1_024 && unit + 1 < UNITS.len() {
        value /= 1_024;
        unit += 1;
    }
    format!("{value} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_userinfo_header() {
        let info = parse_subscription_userinfo(
            "upload=1048576; download=2097152; total=1073741824; expire=1700000000",
        );
        assert_eq!(info.upload_bytes, Some(1_048_576));
        assert_eq!(info.download_bytes, Some(2_097_152));
        assert_eq!(info.total_bytes, Some(1_073_741_824));
        assert_eq!(info.expire_unix, Some(1_700_000_000));
    }

    #[test]
    fn missing_fields_stay_none() {
        let info = parse_subscription_userinfo("total=1073741824");
        assert_eq!(info.total_bytes, Some(1_073_741_824));
        assert_eq!(info.upload_bytes, None);
        assert_eq!(info.expire_unix, None);
    }

    #[test]
    fn unknown_and_malformed_parts_are_ignored() {
        let info = parse_subscription_userinfo("bogus; upload=notanumber; total=100");
        assert_eq!(info.upload_bytes, None);
        assert_eq!(info.total_bytes, Some(100));
    }
}
