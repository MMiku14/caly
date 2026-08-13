//! Length-prefixed frame transport for the local control plane.
//!
//! Every logical message is one frame: a 4-byte little-endian length prefix
//! followed by exactly that many payload bytes. The negotiated
//! `max_message_bytes` ceiling bounds a frame before any allocation, so an
//! oversized or corrupt peer cannot exhaust daemon memory. Payloads are
//! compact JSON (see `wire_frames`).

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Fixed frame header width (u32 little-endian byte length).
pub const HEADER_BYTES: usize = 4;

/// Rejection before payload allocation or interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// Peer closed the connection before a complete frame arrived.
    Closed,
    /// Declared frame length exceeds the transport ceiling.
    Oversized { limit: usize, actual: usize },
    /// Transport-level I/O failure.
    Io,
}

impl FrameError {
    /// Human-readable remediation hint for diagnostics.
    pub fn describe(self) -> String {
        match self {
            Self::Closed => "connection closed by peer".to_owned(),
            Self::Oversized { limit, actual } => {
                format!("frame of {actual} bytes exceeds the {limit}-byte ceiling")
            }
            Self::Io => "transport I/O failure".to_owned(),
        }
    }
}

/// Writes one length-prefixed frame, flushing so a streaming peer sees it.
pub async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::Oversized {
        limit: u32::MAX as usize,
        actual: payload.len(),
    })?;
    writer
        .write_all(&length.to_le_bytes())
        .await
        .map_err(|_| FrameError::Io)?;
    writer
        .write_all(payload)
        .await
        .map_err(|_| FrameError::Io)?;
    writer.flush().await.map_err(|_| FrameError::Io)
}

/// Reads one length-prefixed frame, rejecting declared sizes over the ceiling.
pub async fn read_frame<R>(reader: &mut R, max_bytes: usize) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; HEADER_BYTES];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(FrameError::Closed);
        }
        Err(_) => return Err(FrameError::Io),
    }
    let length = u32::from_le_bytes(header) as usize;
    if length > max_bytes {
        return Err(FrameError::Oversized {
            limit: max_bytes,
            actual: length,
        });
    }
    let mut payload = vec![0_u8; length];
    match reader.read_exact(&mut payload).await {
        Ok(_) => Ok(payload),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Err(FrameError::Closed),
        Err(_) => Err(FrameError::Io),
    }
}

/// Compact JSON encoding of one wire frame.
pub fn encode_json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(value)
}

/// Bounded JSON decoding of one wire frame.
pub fn decode_json<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trip_preserves_payload_bytes() -> Result<(), String> {
        let (mut a, mut b) = duplex(1024);
        write_frame(&mut a, b"hello frame")
            .await
            .map_err(FrameError::describe)?;
        let payload = read_frame(&mut b, 4096)
            .await
            .map_err(FrameError::describe)?;
        assert_eq!(payload, b"hello frame");
        Ok(())
    }

    #[tokio::test]
    async fn zero_length_frame_round_trips() -> Result<(), String> {
        let (mut a, mut b) = duplex(64);
        write_frame(&mut a, b"")
            .await
            .map_err(FrameError::describe)?;
        let payload = read_frame(&mut b, 64).await.map_err(FrameError::describe)?;
        assert!(payload.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_allocation() -> Result<(), String> {
        let (mut a, mut b) = duplex(1024);
        write_frame(&mut a, b"x".repeat(128).as_slice())
            .await
            .map_err(FrameError::describe)?;
        match read_frame(&mut b, 64).await {
            Err(FrameError::Oversized {
                limit: 64,
                actual: 128,
            }) => Ok(()),
            other => Err(format!("expected an oversized rejection, got {other:?}")),
        }
    }

    #[tokio::test]
    async fn clean_close_reports_closed() -> Result<(), String> {
        let (a, mut b) = duplex(64);
        drop(a);
        match read_frame(&mut b, 64).await {
            Err(FrameError::Closed) => Ok(()),
            other => Err(format!("expected a closed rejection, got {other:?}")),
        }
    }
}
