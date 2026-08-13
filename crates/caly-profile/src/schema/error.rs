//! User-facing rendering of configuration failures.

use super::ConfigError;

impl core::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooLarge { limit, actual } => {
                write!(
                    formatter,
                    "configuration is {actual} bytes; reduce it to at most {limit} bytes"
                )
            }
            Self::Parse {
                line,
                column,
                message,
            } => {
                write!(
                    formatter,
                    "configuration parse failed at {line}:{column}: {message}"
                )
            }
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "schema version {version} is unsupported; use version 1"
                )
            }
            Self::InvalidControllerAddress { core, address } => {
                write!(
                    formatter,
                    "{core} controller address `{address}` is invalid; use host:port"
                )
            }
            Self::InvalidRule { index, reason } => {
                write!(formatter, "rules[{index}] is invalid: {reason}")
            }
            Self::InvalidSnifferPort { spec } => write!(
                formatter,
                "sniffer port spec `{spec}` is invalid; use `N` or `N-M` within 1-65535"
            ),
            Self::InvalidDns(error) => write!(formatter, "{error}"),
            Self::InvalidFetchPolicy { field, expected } => write!(
                formatter,
                "subscriptions.{field} is out of range; expected {expected}"
            ),
            Self::UnknownRuleProvider { name, rule_index } => write!(
                formatter,
                "rules[{rule_index}] references rule provider `{name}` which is not declared under rule_providers: and is not auto-emitted by a GEOIP rule"
            ),
            Self::DuplicateRuleProvider { name } => write!(
                formatter,
                "rule_providers: contains duplicate tag `{name}`; tags must be unique"
            ),
            Self::InvalidRuleProviderName { name } => write!(
                formatter,
                "rule_providers: tag `{name}` is not a path-safe identifier (ASCII letters, digits, `-`/`_`/`@`; no separators or `..`)"
            ),
            Self::InvalidProvider { name, reason } => {
                if name.is_empty() {
                    write!(formatter, "providers: {reason}")
                } else {
                    write!(formatter, "providers[{name}] is invalid: {reason}")
                }
            }
            Self::InvalidRuleProviderUrl { name, url } => write!(
                formatter,
                "rule_providers[{name}].url `{url}` is invalid; use a public HTTP(S) URL"
            ),
            Self::EmptyRuleProviderPath { name } => write!(
                formatter,
                "rule_providers[{name}].path must not be empty for type: file"
            ),
            Self::EmptyRuleProviderPayload { name } => write!(
                formatter,
                "rule_providers[{name}].payload must not be empty for type: inline"
            ),
            Self::InvalidRuleProviderInterval { name, interval_ms } => write!(
                formatter,
                "rule_providers[{name}].interval_ms ({interval_ms}) is below the 60s minimum"
            ),
            Self::Profile(error) => write!(formatter, "profile error: {error}"),
            Self::ProxyGroup(error) => write!(formatter, "proxy group error: {error}"),
            _ => formatter.write_str(self.static_message()),
        }
    }
}

impl ConfigError {
    /// Fixed-message variants, kept in one table so `fmt` stays small.
    fn static_message(&self) -> &'static str {
        match self {
            Self::ParseDetailsUnavailable => {
                "configuration parse failed; inspect the configuration syntax"
            }
            Self::InvalidListenAddress => "daemon listen address is invalid; use IP:PORT",
            Self::RemoteListenRequiresTls => "non-loopback daemon listen requires TLS",
            Self::TlsMaterialMissing => {
                "daemon.tls_enabled requires both tls_cert_path and tls_key_path"
            }
            Self::RemoteListenRequiresAuth => {
                "non-loopback daemon listen requires an authentication token"
            }
            Self::InvalidTunMtu => "TUN MTU must be between 576 and 9000",
            Self::InvalidLogLevel => "log.level must be error, warn, info, debug, or trace",
            Self::InvalidTelemetryInterval => "telemetry.interval_ms must be at least 100",
            Self::InvalidSubscriptionUrl => {
                "subscriptions sources must be a public HTTP(S) URL or a file:// path"
            }
            Self::InvalidKernelPort => "kernel.mixed_port must be between 1 and 65535",
            Self::InvalidKernelLogLevel => {
                "kernel.log_level must be trace, debug, info, warn, or error"
            }
            Self::InvalidKernelRestart => {
                "kernel.restart requires initial_backoff_ms >= 100 and max >= initial"
            }
            Self::InvalidKernelTimeout => {
                "kernel.start_timeout_ms and kernel.stop_timeout_ms must be >= 100"
            }
            Self::InvalidTransparentPort => {
                "kernel.transparent.port must be non-zero when transparent is enabled"
            }
            Self::InvalidDnsListen => {
                "dns.listen must be a host:port address (e.g. 127.0.0.1:1053)"
            }
            Self::InvalidDnsCidrFilter => "fallback_filter.ipcidr must be valid CIDR blocks",
            Self::InvalidSystemProxy => {
                "system_proxy.host must be non-empty and system_proxy.port a valid port"
            }
            Self::InvalidTunEscalation => "tun.escalation must be auto, pkexec, sudo, or none",
            Self::TooLarge { .. }
            | Self::Parse { .. }
            | Self::UnsupportedSchemaVersion(_)
            | Self::InvalidControllerAddress { .. }
            | Self::InvalidFetchPolicy { .. }
            | Self::InvalidRule { .. }
            | Self::InvalidSnifferPort { .. }
            | Self::InvalidDns(_)
            | Self::UnknownRuleProvider { .. }
            | Self::DuplicateRuleProvider { .. }
            | Self::InvalidRuleProviderName { .. }
            | Self::InvalidProvider { .. }
            | Self::InvalidRuleProviderUrl { .. }
            | Self::EmptyRuleProviderPath { .. }
            | Self::EmptyRuleProviderPayload { .. }
            | Self::InvalidRuleProviderInterval { .. }
            | Self::Profile(_)
            | Self::ProxyGroup(_) => "parameterized error",
        }
    }
}

impl std::error::Error for ConfigError {}
