use super::*;

#[test]
fn composition_creates_bounded_application_owners() -> Result<(), CompositionError> {
    let composition = ApplicationComposition::new(
        DaemonInstanceId::from_bytes([7; 16]),
        RuntimeCapacities::default(),
        None,
        None,
        caly_domain::Controllers::defaults(),
        1_000,
    )?;
    assert_eq!(
        composition.daemon_instance,
        DaemonInstanceId::from_bytes([7; 16])
    );
    Ok(())
}

#[tokio::test]
async fn runtime_tasks_start_and_shutdown() -> Result<(), CompositionError> {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();

    let composition = ApplicationComposition::new(
        DaemonInstanceId::from_bytes([6; 16]),
        RuntimeCapacities::default(),
        None,
        None,
        caly_domain::Controllers::defaults(),
        1_000,
    )?;
    let runtime = composition.start_runtime(false)?;
    runtime.shutdown().await
}

#[test]
fn unsupported_core_is_rejected_instead_of_falling_back_to_mihomo() {
    assert_eq!(
        parse_configured_core(Some("xray")),
        Err(CompositionError::UnsupportedCore)
    );
    assert_eq!(
        parse_configured_core(Some("unknown")),
        Err(CompositionError::UnsupportedCore)
    );
    assert_eq!(
        parse_configured_core(Some("sing-box")),
        Ok(caly_domain::CoreKind::SingBox)
    );
}

#[test]
fn zero_capacity_is_rejected() {
    assert!(matches!(
        ApplicationComposition::new(
            DaemonInstanceId::from_bytes([8; 16]),
            RuntimeCapacities {
                command_queue: 0,
                ..RuntimeCapacities::default()
            },
            None,
            None,
            caly_domain::Controllers::defaults(),
            1_000,
        ),
        Err(CompositionError::InvalidCapacity)
    ));
}
