//! Isolated one-request template rendering worker.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
use std::{
    io::{self, Read, Write},
    process::ExitCode,
};

use caly_domain::BoundedText;
use caly_template::{
    MAX_TEMPLATE_CONTEXT_BYTES, MAX_TEMPLATE_SOURCE_BYTES, TemplateOutput, WorkerResponse,
    decode_request, encode_response,
};

const MAX_REQUEST_FRAME_BYTES: usize = 64 + MAX_TEMPLATE_SOURCE_BYTES + MAX_TEMPLATE_CONTEXT_BYTES;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::from(2),
    }
}

fn run() -> Result<(), ()> {
    let mut frame = Vec::new();
    io::stdin()
        .take((MAX_REQUEST_FRAME_BYTES as u64) + 1)
        .read_to_end(&mut frame)
        .map_err(|_| ())?;
    if frame.len() > MAX_REQUEST_FRAME_BYTES {
        return Err(());
    }
    let request = decode_request(&frame).map_err(|_| ())?;
    let response = render(request);
    let frame = encode_response(response).map_err(|_| ())?;
    io::stdout().write_all(&frame).map_err(|_| ())?;
    io::stdout().flush().map_err(|_| ())?;
    Ok(())
}

fn render(request: caly_template::TemplateRequest) -> WorkerResponse {
    let request_id = request.request_id;
    let result = render_inner(&request);
    match result {
        Ok(output) => {
            WorkerResponse::Rendered(caly_template::TemplateResponse { request_id, output })
        }
        Err(message) => WorkerResponse::Rejected {
            request_id,
            diagnostic: message,
        },
    }
}

fn render_inner(
    request: &caly_template::TemplateRequest,
) -> Result<TemplateOutput, Option<BoundedText<1_024>>> {
    let source = core::str::from_utf8(request.source.as_slice())
        .map_err(|_| diagnostic("template source is not UTF-8"))?;
    let context: serde_json::Value = serde_json::from_slice(request.context.as_slice())
        .map_err(|_| diagnostic("template context is not valid JSON"))?;
    let rendered = caly_template::render_template(source, &context)
        .map_err(|error| diagnostic(&format!("template render failed: {error}")))?;
    TemplateOutput::try_from_vec(rendered.into_bytes())
        .map_err(|_| diagnostic("template output exceeds configured limit"))
}

fn diagnostic(message: &str) -> Option<BoundedText<1_024>> {
    BoundedText::new(message.to_owned()).ok()
}
