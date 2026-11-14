//! Small private transport frames; none of these values authorize a native root.
use crate::claude::{
    freshness::{Challenge, Snapshot},
    process::ProcessIdentity,
};
use agentchat_core::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::claude::observer) struct Endpoint {
    pub observer: String,
    pub socket: PathBuf,
    pub pid: u32,
    #[serde(default)]
    pub owner: Option<ProcessIdentity>,
    #[serde(default)]
    pub retire_token: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<super::ownership::Ownership>,
}

/// A native-owned stream receives only a nonce, ordering value and reply path.
/// The API/root snapshot must be obtained independently, never from this input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// One outstanding unpredictable native observation challenge.
    pub nonce: Uuid,
    /// Relay-local ordering; never substitutes for the native sequence.
    pub relay_sequence: u64,
    /// Private short reply socket, used by the native answer helper.
    pub reply_socket: PathBuf,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Request {
    Observe(Challenge),
    Retire(Retire),
    RefreshContext(ContextRefresh),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContextRefresh {
    pub refresh_context: Uuid,
    pub native_generation: u64,
    pub launch_generation: Uuid,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Retire {
    pub retire: Uuid,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub nonce: Uuid,
    pub accepted: bool,
}

// Take the bounded stream before read_until allocates. A newline beyond the
// limit is refused; a stalled peer is bounded by the caller's shared deadline.
pub(super) async fn read<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: R,
    limit: usize,
) -> Result<T> {
    let mut reader = BufReader::new(reader.take((limit + 1) as u64));
    let mut bytes = Vec::new();
    reader
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|_| super::unavailable())?;
    if bytes.len() > limit || !bytes.ends_with(b"\n") {
        return Err(super::unavailable());
    }
    serde_json::from_slice(&bytes).map_err(|_| super::unavailable())
}

pub(super) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    struct Limited(Vec<u8>);
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > 8191 - self.0.len() {
                return Err(std::io::Error::other("Native frame bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bounded = Limited(Vec::new());
    serde_json::to_writer(&mut bounded, value).map_err(|_| super::unavailable())?;
    bounded.0.push(b'\n');
    Ok(bounded.0)
}
pub(super) async fn write<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<()> {
    writer
        .write_all(&encode(value)?)
        .await
        .map_err(|_| super::unavailable())?;
    writer.flush().await.map_err(|_| super::unavailable())
}

pub(super) fn answer_matches(answer: &Snapshot, question: &Question, observer: &str) -> bool {
    answer.nonce == question.nonce
        && answer.relay_sequence == question.relay_sequence
        && answer.observer == observer
}
