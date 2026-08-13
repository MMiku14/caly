//! Central Application-to-Transport error mapping.

use caly_application::service::ApplicationServiceError;

use super::ServiceError;

pub fn service_error(value: ApplicationServiceError) -> ServiceError {
    match value {
        ApplicationServiceError::ResourceExhausted => ServiceError::ResourceExhausted,
        ApplicationServiceError::OperationNotFound => ServiceError::InvalidArgument {
            reason: "operation was not found; verify the operation id".to_owned(),
        },
        ApplicationServiceError::IdempotencyConflict => ServiceError::InvalidArgument {
            reason: "operation id was already used for a different command".to_owned(),
        },
        ApplicationServiceError::InvalidCommand(reason) => ServiceError::InvalidArgument {
            reason: reason.as_str().to_owned(),
        },
        ApplicationServiceError::Unavailable | ApplicationServiceError::InternalInvariant => {
            ServiceError::ApplicationUnavailable
        }
    }
}
