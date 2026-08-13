//! Cross-field validation tests for kernel/tun schema sections.

use crate::schema::*;

#[test]
fn restart_bounds_are_validated() {
    let tiny = br#"{"schema_version":1,"core":"mihomo","kernel":{"restart":{"initial_backoff_ms":10,"max_backoff_ms":100}}}"#;
    assert!(matches!(
        parse_and_validate_json(tiny),
        Err(ConfigError::InvalidKernelRestart)
    ));
    let inverted = br#"{"schema_version":1,"core":"mihomo","kernel":{"restart":{"initial_backoff_ms":5000,"max_backoff_ms":1000}}}"#;
    assert!(matches!(
        parse_and_validate_json(inverted),
        Err(ConfigError::InvalidKernelRestart)
    ));
}

#[test]
fn tun_escalation_is_validated() {
    let bad =
        br#"{"schema_version":1,"core":"mihomo","tun":{"enabled":false,"escalation":"doas"}}"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidTunEscalation)
    ));
    let good =
        br#"{"schema_version":1,"core":"mihomo","tun":{"enabled":false,"escalation":"sudo"}}"#;
    assert!(parse_and_validate_json(good).is_ok());
}

#[test]
fn valid_rules_are_accepted_and_invalid_rejected() {
    let good = br#"{"schema_version":1,"core":"mihomo","rules":["DOMAIN-SUFFIX,google.com,PROXY","MATCH,PROXY"]}"#;
    assert!(parse_and_validate_json(good).is_ok());
    let bad = br#"{"schema_version":1,"core":"mihomo","rules":["BOGUS,foo,DIRECT"]}"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidRule { .. })
    ));
}

#[test]
fn transparent_zero_port_is_rejected_when_enabled() {
    let bad = br#"{"schema_version":1,"core":"mihomo","kernel":{"transparent":{"enabled":true,"mode":"redirect","port":0}}}"#;
    assert!(matches!(
        parse_and_validate_json(bad),
        Err(ConfigError::InvalidTransparentPort)
    ));
    let good = br#"{"schema_version":1,"core":"mihomo","kernel":{"transparent":{"enabled":true,"mode":"tproxy","port":7892}}}"#;
    assert!(parse_and_validate_json(good).is_ok());
}
