//! Versioned bounded binary framing for the template worker process.

use caly_domain::BoundedText;

use super::{
    MAX_TEMPLATE_CONTEXT_BYTES, MAX_TEMPLATE_OUTPUT_BYTES, MAX_TEMPLATE_SOURCE_BYTES,
    TemplateContext, TemplateOutput, TemplateRequest, TemplateResponse, TemplateSource,
};

const REQUEST_MAGIC: &[u8; 4] = b"CTWR";
const RESPONSE_MAGIC: &[u8; 4] = b"CTWS";
const FRAME_VERSION: u16 = 1;
const REQUEST_HEADER_BYTES: usize = 4 + 2 + 16 + 4 + 4;
const RESPONSE_HEADER_BYTES: usize = 4 + 2 + 16 + 1 + 4;

/// Worker response including bounded render diagnostics.
pub enum WorkerResponse {
    Rendered(TemplateResponse),
    Rejected {
        request_id: [u8; 16],
        diagnostic: Option<BoundedText<1_024>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    TooShort,
    InvalidMagic,
    UnsupportedVersion,
    InvalidStatus,
    LengthOverflow,
    LengthMismatch,
    SourceTooLarge,
    ContextTooLarge,
    OutputTooLarge,
    DiagnosticRejected,
}

pub fn encode_request(request: TemplateRequest) -> Result<Vec<u8>, FrameError> {
    let source_len = u32::try_from(request.source.len()).map_err(|_| FrameError::LengthOverflow)?;
    let context_len =
        u32::try_from(request.context.len()).map_err(|_| FrameError::LengthOverflow)?;
    let capacity = REQUEST_HEADER_BYTES
        .checked_add(request.source.len())
        .and_then(|value| value.checked_add(request.context.len()))
        .ok_or(FrameError::LengthOverflow)?;
    let mut frame = Vec::with_capacity(capacity);
    frame.extend_from_slice(REQUEST_MAGIC);
    frame.extend_from_slice(&FRAME_VERSION.to_be_bytes());
    frame.extend_from_slice(&request.request_id);
    frame.extend_from_slice(&source_len.to_be_bytes());
    frame.extend_from_slice(&context_len.to_be_bytes());
    frame.extend_from_slice(request.source.as_slice());
    frame.extend_from_slice(request.context.as_slice());
    Ok(frame)
}

pub fn decode_request(frame: &[u8]) -> Result<TemplateRequest, FrameError> {
    if frame.len() < REQUEST_HEADER_BYTES {
        return Err(FrameError::TooShort);
    }
    if &frame[..4] != REQUEST_MAGIC {
        return Err(FrameError::InvalidMagic);
    }
    if read_u16(frame, 4)? != FRAME_VERSION {
        return Err(FrameError::UnsupportedVersion);
    }
    let request_id = read_id(frame, 6)?;
    let source_len = read_u32(frame, 22)? as usize;
    let context_len = read_u32(frame, 26)? as usize;
    if source_len > MAX_TEMPLATE_SOURCE_BYTES {
        return Err(FrameError::SourceTooLarge);
    }
    if context_len > MAX_TEMPLATE_CONTEXT_BYTES {
        return Err(FrameError::ContextTooLarge);
    }
    let source_end = REQUEST_HEADER_BYTES
        .checked_add(source_len)
        .ok_or(FrameError::LengthOverflow)?;
    let frame_end = source_end
        .checked_add(context_len)
        .ok_or(FrameError::LengthOverflow)?;
    if frame_end != frame.len() {
        return Err(FrameError::LengthMismatch);
    }
    let source = TemplateSource::try_from_vec(frame[REQUEST_HEADER_BYTES..source_end].to_vec())
        .map_err(|_| FrameError::SourceTooLarge)?;
    let context = TemplateContext::try_from_vec(frame[source_end..frame_end].to_vec())
        .map_err(|_| FrameError::ContextTooLarge)?;
    Ok(TemplateRequest {
        request_id,
        source,
        context,
    })
}

pub fn encode_response(response: WorkerResponse) -> Result<Vec<u8>, FrameError> {
    match response {
        WorkerResponse::Rendered(response) => {
            encode_response_payload(response.request_id, 0, response.output.as_slice())
        }
        WorkerResponse::Rejected {
            request_id,
            diagnostic,
        } => encode_response_payload(
            request_id,
            1,
            diagnostic
                .as_ref()
                .map_or(&[], |value| value.as_str().as_bytes()),
        ),
    }
}

pub fn decode_response(frame: &[u8]) -> Result<WorkerResponse, FrameError> {
    if frame.len() < RESPONSE_HEADER_BYTES {
        return Err(FrameError::TooShort);
    }
    if &frame[..4] != RESPONSE_MAGIC {
        return Err(FrameError::InvalidMagic);
    }
    if read_u16(frame, 4)? != FRAME_VERSION {
        return Err(FrameError::UnsupportedVersion);
    }
    let request_id = read_id(frame, 6)?;
    let status = frame[22];
    let length = read_u32(frame, 23)? as usize;
    let end = RESPONSE_HEADER_BYTES
        .checked_add(length)
        .ok_or(FrameError::LengthOverflow)?;
    if end != frame.len() {
        return Err(FrameError::LengthMismatch);
    }
    match status {
        0 => {
            let output = TemplateOutput::try_from_vec(frame[RESPONSE_HEADER_BYTES..].to_vec())
                .map_err(|_| FrameError::OutputTooLarge)?;
            Ok(WorkerResponse::Rendered(TemplateResponse {
                request_id,
                output,
            }))
        }
        1 => {
            let text = core::str::from_utf8(&frame[RESPONSE_HEADER_BYTES..])
                .map_err(|_| FrameError::DiagnosticRejected)?;
            let diagnostic = if text.is_empty() {
                None
            } else {
                Some(
                    BoundedText::new(text.to_owned())
                        .map_err(|_| FrameError::DiagnosticRejected)?,
                )
            };
            Ok(WorkerResponse::Rejected {
                request_id,
                diagnostic,
            })
        }
        _ => Err(FrameError::InvalidStatus),
    }
}

fn encode_response_payload(
    request_id: [u8; 16],
    status: u8,
    payload: &[u8],
) -> Result<Vec<u8>, FrameError> {
    if status == 0 && payload.len() > MAX_TEMPLATE_OUTPUT_BYTES {
        return Err(FrameError::OutputTooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::LengthOverflow)?;
    let mut frame = Vec::with_capacity(RESPONSE_HEADER_BYTES + payload.len());
    frame.extend_from_slice(RESPONSE_MAGIC);
    frame.extend_from_slice(&FRAME_VERSION.to_be_bytes());
    frame.extend_from_slice(&request_id);
    frame.push(status);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

fn read_u16(frame: &[u8], offset: usize) -> Result<u16, FrameError> {
    let bytes = frame.get(offset..offset + 2).ok_or(FrameError::TooShort)?;
    let bytes: [u8; 2] = bytes.try_into().map_err(|_| FrameError::TooShort)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(frame: &[u8], offset: usize) -> Result<u32, FrameError> {
    let bytes = frame.get(offset..offset + 4).ok_or(FrameError::TooShort)?;
    let bytes: [u8; 4] = bytes.try_into().map_err(|_| FrameError::TooShort)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_id(frame: &[u8], offset: usize) -> Result<[u8; 16], FrameError> {
    frame
        .get(offset..offset + 16)
        .ok_or(FrameError::TooShort)?
        .try_into()
        .map_err(|_| FrameError::TooShort)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_preserves_id_and_payload() -> Result<(), FrameError> {
        let request = TemplateRequest {
            request_id: [7; 16],
            source: TemplateSource::try_from_vec(b"hello {{ name }}".to_vec())
                .map_err(|_| FrameError::SourceTooLarge)?,
            context: TemplateContext::try_from_vec(br#"{"name":"caly"}"#.to_vec())
                .map_err(|_| FrameError::ContextTooLarge)?,
        };
        let decoded = decode_request(&encode_request(request)?)?;
        assert_eq!(decoded.request_id, [7; 16]);
        assert_eq!(decoded.source.as_slice(), b"hello {{ name }}");
        Ok(())
    }

    #[test]
    fn response_rejects_trailing_bytes() -> Result<(), FrameError> {
        let response = WorkerResponse::Rendered(TemplateResponse {
            request_id: [1; 16],
            output: TemplateOutput::try_from_vec(b"ok".to_vec())
                .map_err(|_| FrameError::OutputTooLarge)?,
        });
        let mut frame = encode_response(response)?;
        frame.push(0);
        assert_eq!(
            decode_response(&frame).err(),
            Some(FrameError::LengthMismatch)
        );
        Ok(())
    }
}
