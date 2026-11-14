//! Bounded newline-delimited JSON; allocation is limited before deserialization.

use agentchat_core::{Error, ErrorCode, Result};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum wire frame, including its trailing newline.
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// Read one complete frame or clean EOF, rejecting truncated and oversized input.
///
/// `fill_buf` reads a bounded borrowed chunk. We never call unbounded `lines` or
/// `read_until` and then check an already-allocated oversized attacker payload.
/// Discard the connection if this convenience future is cancelled mid-frame;
/// retain a `FramedReader` when reads must survive cancellation.
pub async fn read<R: AsyncBufRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<T>> {
    FramedReader::new(reader).read().await
}

/// Bounded incremental decoder retaining partial bytes across cancelled reads.
pub struct FramedReader<R> {
    inner: R,
    bytes: Vec<u8>,
    failed: bool,
}
impl<R: AsyncBufRead + Unpin> FramedReader<R> {
    /// Wrap an existing buffered connection without reading or allocating a frame.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            bytes: Vec::new(),
            failed: false,
        }
    }
    /// Read a complete frame. Cancellation preserves accumulated bytes; a framing
    /// failure permanently rejects reuse, so malformed data cannot resynchronize.
    pub async fn read<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        if self.failed {
            return Err(invalid(
                "Framed connection failed; reconnect before reading.",
            ));
        }
        let result = self.decode().await;
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    async fn decode<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        loop {
            let chunk = self.inner.fill_buf().await.map_err(io)?;
            if chunk.is_empty() {
                return if self.bytes.is_empty() {
                    Ok(None)
                } else {
                    Err(invalid("Truncated JSON frame."))
                };
            }
            let end = chunk.iter().position(|&c| c == b'\n').map(|i| i + 1);
            let count = end.unwrap_or(chunk.len());
            if count > MAX_FRAME_BYTES - self.bytes.len() {
                return Err(invalid("JSON frame exceeds 1 MiB."));
            }
            // No await between retaining bytes and consuming the same chunk.
            // Cancellation at the next fill_buf can never lose a frame prefix.
            self.bytes.extend_from_slice(&chunk[..count]);
            self.inner.consume(count);
            if end.is_some() {
                let bytes = std::mem::take(&mut self.bytes);
                return serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|_| invalid("Invalid UTF-8 JSON frame."));
            }
        }
    }
}

/// Serialize and write a bounded JSON frame; oversized results must be paginated.
pub async fn write<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    // Serialization itself is bounded too, including accidental large results.
    // A limiting writer avoids allocating an oversized Vec before detecting it.
    let mut bounded = BoundedBuffer(Vec::new());
    serde_json::to_writer(&mut bounded, value)
        .map_err(|_| invalid("Response exceeds the JSON frame budget."))?;
    bounded.0.push(b'\n');
    writer.write_all(&bounded.0).await.map_err(io)?;
    writer.flush().await.map_err(io)
}

struct BoundedBuffer(Vec<u8>);

impl std::io::Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() >= MAX_FRAME_BYTES - self.0.len() {
            return Err(std::io::Error::other("frame budget exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}

fn io(_: std::io::Error) -> Error {
    Error::new(ErrorCode::IoError, "Local transport I/O failed.")
}
