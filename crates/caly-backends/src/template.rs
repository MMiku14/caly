//! Hard-timeout template rendering through the owned worker process.
//!
//! Wires the `TemplateWorker` contract (implemented by `LinuxTemplateWorker`
//! in caly-profile) into the application. Template rendering runs in a child
//! process with a wall-clock timeout so a pathological template cannot stall
//! or crash the daemon.

use std::path::PathBuf;
use std::time::Duration;

use caly_domain::BoundedText;
use caly_template::{
    LinuxTemplateWorker, TemplateContext, TemplateRequest, TemplateResponse, TemplateSource,
    TemplateWorker, TemplateWorkerFailure, TemplateWorkerPlan,
};

use caly_ports::ActorFailure;

/// Worker executable resolution precedence: env override, then a vendored path.
pub fn default_worker_executable() -> PathBuf {
    std::env::var_os("CALY_TEMPLATE_WORKER_BIN").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bin/caly-template-worker"),
        PathBuf::from,
    )
}

/// Bounded template render request, ready to be framed to the worker.
pub struct TemplateRenderInput {
    request_id: [u8; 16],
    source: Vec<u8>,
    context: Vec<u8>,
}

impl TemplateRenderInput {
    /// Constructs a bounded render request.
    pub fn new(
        request_id: [u8; 16],
        source: Vec<u8>,
        context: Vec<u8>,
    ) -> Result<Self, ActorFailure> {
        let _ = TemplateSource::try_from_vec(source.clone()).map_err(|_| {
            crate::failure(
                "template source exceeds the bounded capacity",
                "reduce the template source size",
            )
        })?;
        let _ = TemplateContext::try_from_vec(context.clone()).map_err(|_| {
            crate::failure(
                "template context exceeds the bounded capacity",
                "reduce the template context size",
            )
        })?;
        Ok(Self {
            request_id,
            source,
            context,
        })
    }

    /// Builds the framed worker request.
    fn into_request(self) -> Result<TemplateRequest, ActorFailure> {
        Ok(TemplateRequest {
            request_id: self.request_id,
            source: TemplateSource::try_from_vec(self.source).map_err(|_| {
                crate::failure(
                    "template source exceeds the bounded capacity",
                    "reduce the template source size",
                )
            })?,
            context: TemplateContext::try_from_vec(self.context).map_err(|_| {
                crate::failure(
                    "template context exceeds the bounded capacity",
                    "reduce the template context size",
                )
            })?,
        })
    }
}

/// Template rendering backend backed by the worker process.
#[derive(Default)]
pub struct TemplateRenderBackend {
    worker: LinuxTemplateWorker,
}

impl TemplateRenderBackend {
    /// Renders `source` with `context`, bounding wall time via the worker.
    pub fn render(
        &mut self,
        input: TemplateRenderInput,
        executable: PathBuf,
        timeout: Duration,
    ) -> Result<Vec<u8>, ActorFailure> {
        let plan = TemplateWorkerPlan {
            executable,
            wall_timeout: timeout,
            worker_name: BoundedText::new("caly-template-worker".to_owned())
                .map_err(|_| crate::failure("worker name is invalid", "inspect worker naming"))?,
        };
        let request = input.into_request()?;
        let response: TemplateResponse =
            self.worker.render(&plan, request).map_err(worker_error)?;
        Ok(response.output.into_vec())
    }
}

/// Maps a worker failure to a daemon-safe actor failure.
fn worker_error(error: TemplateWorkerFailure) -> ActorFailure {
    let message = match error {
        TemplateWorkerFailure::Spawn(_) => "template worker could not be spawned",
        TemplateWorkerFailure::WriteRequest(_) => "template worker request write failed",
        TemplateWorkerFailure::ReadResponse(_) => "template worker response read failed",
        TemplateWorkerFailure::TimedOut { .. } => "template worker exceeded its wall timeout",
        TemplateWorkerFailure::InvalidResponse(_) => "template worker returned an invalid response",
    };
    crate::failure(
        message,
        "inspect the worker executable and template request",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_accepts_bounded_source_and_context() -> Result<(), ActorFailure> {
        let input = TemplateRenderInput::new(
            [1; 16],
            b"hello {{ name }}".to_vec(),
            br#"{"name":"caly"}"#.to_vec(),
        )?;
        let request = input.into_request()?;
        assert_eq!(request.request_id, [1; 16]);
        assert_eq!(request.source.as_slice(), b"hello {{ name }}");
        Ok(())
    }

    #[test]
    fn oversized_source_is_rejected() {
        let huge = vec![b'x'; caly_template::MAX_TEMPLATE_SOURCE_BYTES + 1];
        let result = TemplateRenderInput::new([2; 16], huge, b"{}".to_vec());
        assert!(result.is_err());
    }

    #[test]
    fn oversized_context_is_rejected() {
        let huge = vec![b'x'; caly_template::MAX_TEMPLATE_CONTEXT_BYTES + 1];
        let result = TemplateRenderInput::new([3; 16], b"x".to_vec(), huge);
        assert!(result.is_err());
    }
}
