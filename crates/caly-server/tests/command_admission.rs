//! Cross-layer command admission matrix for the current daemon MVP.

use caly_application::{
    command_bus::{CommandReceiver, command_bus},
    operations::{AdmissionController, OperationStore, TimeSource},
    service::{
        ApplicationServiceError, ApplicationWatch, CommandSupportPolicy, ProjectionService,
        RuntimeService,
    },
};
use caly_domain::{EventCursor, PresentationSnapshot, UnixMillis};
use caly_protocol::protocol::v2::{
    DecodeLimits, ExecuteRequest, FeatureList, GetOperationStatusRequest, HandshakeRequest,
    ProtocolVersion, WireCommand,
};
use caly_server::json::{ServiceAdapter, ServiceError, ServiceV2};

struct Clock(u64);

impl TimeSource for Clock {
    fn now(&mut self) -> UnixMillis {
        self.0 = self.0.saturating_add(1);
        UnixMillis::new(self.0)
    }
}

struct Projection;

impl ProjectionService for Projection {
    fn snapshot(&self) -> Result<PresentationSnapshot, ApplicationServiceError> {
        Err(ApplicationServiceError::Unavailable)
    }

    fn watch_after(
        &self,
        _cursor: Option<EventCursor>,
    ) -> Result<ApplicationWatch, ApplicationServiceError> {
        Err(ApplicationServiceError::Unavailable)
    }

    fn subscribe_live(
        &self,
    ) -> tokio::sync::broadcast::Receiver<caly_application::events::SequencedEvent> {
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);
        rx
    }
}

type TestAdapter = ServiceAdapter<RuntimeService<Clock, Projection>>;

fn adapter() -> Result<(TestAdapter, CommandReceiver), String> {
    let (ingress, receiver) = command_bus(16).map_err(|error| error.to_string())?;
    let store = OperationStore::new(32, 16).map_err(|error| error.to_string())?;
    let admission = AdmissionController::new(store, ingress, Clock(0));
    let service = RuntimeService::new_with_policy(
        admission,
        Projection,
        CommandSupportPolicy::lifecycle_only(caly_domain::CoreKind::Mihomo),
    );
    let mut adapter = ServiceAdapter::new(
        service,
        [0; 16],
        [0; 16],
        caly_server::admission::TokenAdmission::Open,
        FeatureList::new(),
        DecodeLimits::v2_default(),
    );
    adapter
        .handshake(HandshakeRequest {
            client_version: ProtocolVersion::V2_0,
            requested_features: FeatureList::new(),
            auth_token: None,
        })
        .map_err(|error| format!("handshake failed: {error:?}"))?;
    Ok((adapter, receiver))
}

#[test]
fn only_active_core_lifecycle_reaches_operation_admission() -> Result<(), String> {
    let (mut adapter, _receiver) = adapter()?;
    let supported = adapter.execute(ExecuteRequest {
        operation_id: [1; 16],
        command: WireCommand::SwitchCore {
            core_kind: 1,
            action: 1,
        },
    });
    if supported.is_err() {
        return Err(format!(
            "active lifecycle command was rejected: {supported:?}"
        ));
    }

    let unsupported = [
        WireCommand::ApplyConfig {
            candidate_id: [3; 16],
        },
        WireCommand::SelectProxy { node_id: [4; 16] },
        WireCommand::SetMode { mode: 2 },
        WireCommand::SetTun { enabled: true },
        WireCommand::SetSystemProxy { enabled: true },
        WireCommand::RefreshSubscription {
            subscription_id: [5; 16],
            force: false,
        },
        WireCommand::CloseAllConnections,
    ];
    for (index, command) in unsupported.into_iter().enumerate() {
        let byte = u8::try_from(index + 10).map_err(|error| error.to_string())?;
        let operation_id = [byte; 16];
        let result = adapter.execute(ExecuteRequest {
            operation_id,
            command,
        });
        if !matches!(result, Err(ServiceError::InvalidArgument { .. })) {
            return Err(format!(
                "unsupported command did not fail at admission: {result:?}"
            ));
        }
        let status = adapter.status(GetOperationStatusRequest { operation_id });
        if !matches!(status, Err(ServiceError::InvalidArgument { .. })) {
            return Err("rejected command unexpectedly reserved an operation".to_owned());
        }
    }
    Ok(())
}

#[test]
fn switch_to_any_managed_core_is_admissible() -> Result<(), String> {
    let (mut adapter, _receiver) = adapter()?;
    // sing-box target (2) with restart action (3) is accepted now that both
    // kernels are pre-built; Xray (3) stays rejected.
    let result = adapter.execute(ExecuteRequest {
        operation_id: [9; 16],
        command: WireCommand::SwitchCore {
            core_kind: 2,
            action: 3,
        },
    });
    assert!(
        result.is_ok(),
        "dual-core switch must pass admission: {result:?}"
    );
    let xray = adapter.execute(ExecuteRequest {
        operation_id: [10; 16],
        command: WireCommand::SwitchCore {
            core_kind: 3,
            action: 3,
        },
    });
    assert!(matches!(xray, Err(ServiceError::InvalidArgument { .. })));
    Ok(())
}

/// #58: a token-enforcing adapter rejects a handshake without
/// (or with a wrong) `auth_token` before any negotiation, and
/// admits the exact token. The UDS path keeps `Open` admission
/// (its guard is the socket's permissions + peer credentials).
#[test]
fn handshake_admission_enforces_the_configured_token() -> Result<(), String> {
    let build = |admission: caly_server::admission::TokenAdmission| -> Result<TestAdapter, String> {
        let (ingress, _receiver) = command_bus(16).map_err(|error| error.to_string())?;
        let store = OperationStore::new(32, 16).map_err(|error| error.to_string())?;
        let controller = AdmissionController::new(store, ingress, Clock(0));
        let service = RuntimeService::new_with_policy(
            controller,
            Projection,
            CommandSupportPolicy::lifecycle_only(caly_domain::CoreKind::Mihomo),
        );
        Ok(ServiceAdapter::new(
            service,
            [0; 16],
            [9; 16],
            admission,
            FeatureList::new(),
            DecodeLimits::v2_default(),
        ))
    };
    let request = |auth_token: Option<String>| HandshakeRequest {
        client_version: ProtocolVersion::V2_0,
        requested_features: FeatureList::new(),
        auth_token,
    };

    let mut guarded = build(caly_server::admission::TokenAdmission::Token(
        "s3cret".to_owned(),
    ))?;
    assert!(matches!(
        guarded.handshake(request(None)),
        Err(ServiceError::Unauthenticated)
    ));
    assert!(matches!(
        guarded.handshake(request(Some("wrong".to_owned()))),
        Err(ServiceError::Unauthenticated)
    ));
    assert!(
        guarded
            .handshake(request(Some("s3cret".to_owned())))
            .is_ok(),
        "the exact configured token must pass admission"
    );

    let mut open = build(caly_server::admission::TokenAdmission::Open)?;
    assert!(open.handshake(request(None)).is_ok());
    Ok(())
}
