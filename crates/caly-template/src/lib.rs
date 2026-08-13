//! Hard-timeout template rendering through an owned worker process.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
mod framing;
mod linux;
mod render;

pub use linux::LinuxTemplateWorker;

pub use framing::{
    FrameError, WorkerResponse, decode_request, decode_response, encode_request, encode_response,
};

pub use render::{TemplateRenderError, render_template};

use std::{path::PathBuf, time::Duration};

use caly_domain::{BoundedText, BoundedVec};
use caly_platform::PlatformFailure;

pub const MAX_TEMPLATE_SOURCE_BYTES: usize = 4 * 1_024 * 1_024;
pub const MAX_TEMPLATE_CONTEXT_BYTES: usize = 16 * 1_024 * 1_024;
pub const MAX_TEMPLATE_OUTPUT_BYTES: usize = 16 * 1_024 * 1_024;
pub type TemplateSource = BoundedVec<u8, MAX_TEMPLATE_SOURCE_BYTES>;
pub type TemplateContext = BoundedVec<u8, MAX_TEMPLATE_CONTEXT_BYTES>;
pub type TemplateOutput = BoundedVec<u8, MAX_TEMPLATE_OUTPUT_BYTES>;

/// Upper bound on a serialized request frame: fixed header + both payloads.
pub const MAX_REQUEST_FRAME_BYTES: usize =
    64 + MAX_TEMPLATE_SOURCE_BYTES + MAX_TEMPLATE_CONTEXT_BYTES;
/// Upper bound on a serialized response frame: fixed header + output payload.
pub const MAX_RESPONSE_FRAME_BYTES: usize = 64 + MAX_TEMPLATE_OUTPUT_BYTES;

/// Worker request transmitted over bounded stdin.
pub struct TemplateRequest {
    pub request_id: [u8; 16],
    pub source: TemplateSource,
    pub context: TemplateContext,
}

/// Worker response read from bounded stdout.
pub struct TemplateResponse {
    pub request_id: [u8; 16],
    pub output: TemplateOutput,
}

/// Process-based isolation plan; thread abort is not permitted.
pub struct TemplateWorkerPlan {
    pub executable: PathBuf,
    pub wall_timeout: Duration,
    pub worker_name: BoundedText<64>,
}

/// Backend must spawn contained process, bound stdin/stdout, timeout, kill tree and reap.
pub trait TemplateWorker {
    fn render(
        &mut self,
        plan: &TemplateWorkerPlan,
        request: TemplateRequest,
    ) -> Result<TemplateResponse, TemplateWorkerFailure>;
}

/// Worker failure states the lifecycle step and preserves platform context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateWorkerFailure {
    Spawn(PlatformFailure),
    WriteRequest(PlatformFailure),
    ReadResponse(PlatformFailure),
    TimedOut {
        kill: Option<PlatformFailure>,
        reap: Option<PlatformFailure>,
    },
    InvalidResponse(BoundedText<1_024>),
}
