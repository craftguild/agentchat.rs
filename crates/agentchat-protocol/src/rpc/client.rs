//! Checked local RPC client; reconnect/retry policy retains the mutation key.

use super::{Call, Id, ReplyBody, Request, TypedReply, invalid, unavailable};
use crate::{
    envelope::{Envelope, Handshake, Outcome, VERSION},
    framing,
};
use agentchat_core::{Error, ErrorCode, Result};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::Path,
    time::Duration,
};
use tokio::{
    io::BufReader,
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::timeout,
};
use uuid::Uuid;

/// One checked, sequential connection. Server binding prevents context switching.
pub struct Client {
    pub(super) reader: framing::FramedReader<BufReader<OwnedReadHalf>>,
    pub(super) writer: OwnedWriteHalf,
    pub(super) project: Uuid,
    instance: Uuid,
    poisoned: bool,
}
impl Client {
    /// Connect to a same-user private socket and verify project/protocol identity.
    pub async fn connect(path: impl AsRef<Path>, project: Uuid) -> Result<Self> {
        timeout(
            Duration::from_secs(5),
            Self::connect_checked(path.as_ref(), project),
        )
        .await
        .map_err(|_| unavailable("Runtime connection and handshake timed out."))?
    }
    async fn connect_checked(path: &Path, project: Uuid) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("Socket filename has no parent."))?;
        let directory = std::fs::symlink_metadata(parent)
            .map_err(|_| unavailable("Runtime socket directory is unavailable."))?;
        let uid = rustix::process::geteuid().as_raw();
        if !directory.is_dir()
            || directory.file_type().is_symlink()
            || directory.uid() != uid
            || directory.permissions().mode() & 0o777 != 0o700
        {
            return Err(Error::new(
                ErrorCode::ConfigurationConflict,
                "Runtime directory is not private and owned by this user.",
            ));
        }
        let socket = std::fs::symlink_metadata(path)
            .map_err(|_| unavailable("Runtime socket is unavailable."))?;
        if !socket.file_type().is_socket() || socket.uid() != uid {
            return Err(invalid("Runtime endpoint is not an owned Unix socket."));
        }
        let stream = timeout(Duration::from_secs(3), UnixStream::connect(path))
            .await
            .map_err(|_| unavailable("Runtime connection timed out."))?
            .map_err(|_| unavailable("Runtime connection failed."))?;
        if stream
            .peer_cred()
            .map_err(|_| invalid("Peer credentials are unavailable."))?
            .uid()
            != uid
        {
            return Err(Error::new(
                ErrorCode::ConfigurationConflict,
                "Runtime peer belongs to another OS user.",
            ));
        }
        let (reader, writer) = stream.into_split();
        let mut client = Self {
            reader: framing::FramedReader::new(BufReader::new(reader)),
            writer,
            project,
            instance: Uuid::nil(),
            poisoned: false,
        };
        let envelope = timeout(
            Duration::from_secs(5),
            client.call_as::<HandshakeResult>(
                Uuid::new_v4(),
                Call::Handshake(Handshake {
                    version: VERSION,
                    project_id: project,
                }),
            ),
        )
        .await
        .map_err(|_| unavailable("Runtime handshake timed out."))??;
        let value = success(envelope)?;
        if value.version != VERSION || value.project_id != project {
            return Err(invalid(
                "Runtime handshake payload does not match the requested namespace.",
            ));
        }
        client.instance = value.instance_id;
        let current = std::fs::symlink_metadata(path)
            .map_err(|_| unavailable("Runtime socket disappeared during handshake."))?;
        if current.ino() != socket.ino()
            || current.dev() != socket.dev()
            || current.permissions().mode() & 0o777 != 0o600
        {
            return Err(invalid(
                "Runtime socket changed identity or lacks private permissions.",
            ));
        }
        Ok(client)
    }
    /// Actual checked daemon incarnation, never inferred from its PID.
    pub fn instance_id(&self) -> Uuid {
        self.instance
    }
    /// Invoke one finite non-read operation with explicit request correlation.
    /// Reads use `read`/`query` and cannot erase their response through this API.
    /// Mutation calls keep their own stable semantic request_id across reconnects.
    pub async fn call(&mut self, id: Uuid, call: Call) -> Result<Envelope<Value>> {
        if matches!(
            call,
            Call::Query { .. } | Call::NativeBinding { .. } | Call::InternalHealth
        ) || call.dedicated()
        {
            return Err(invalid(
                "Read calls require Client::read or Client::query to retain their response type.",
            ));
        }
        self.call_as(id, call).await
    }
    pub(super) async fn call_as<T: DeserializeOwned>(
        &mut self,
        id: Uuid,
        call: Call,
    ) -> Result<Envelope<T>> {
        if self.poisoned {
            return Err(unavailable("Reconnect after an incomplete RPC exchange."));
        }
        let key = call.result_key(id);
        let request = Request::new(id, &call)?;
        // Cancellation or an I/O failure leaves this set. A later call cannot
        // mistake the earlier operation's response for its own result.
        self.poisoned = true;
        timeout(
            Duration::from_secs(5),
            framing::write(&mut self.writer, &request),
        )
        .await
        .map_err(|_| unavailable("RPC write timed out."))??;
        let reply: TypedReply<T> = timeout(Duration::from_secs(30), self.reader.read())
            .await
            .map_err(|_| unavailable("RPC response timed out."))??
            .ok_or_else(|| unavailable("Runtime closed before responding."))?;
        if reply.jsonrpc != "2.0" || reply.id != Some(Id::Text(id.to_string())) {
            return Err(invalid("RPC response correlation is invalid."));
        }
        match reply.body {
            ReplyBody::Failure { error } => {
                self.poisoned = false;
                Err(error.data)
            }
            ReplyBody::Success { result } => {
                if result.version != VERSION
                    || result.project_id != Some(self.project)
                    || result.request_id != key
                {
                    return Err(invalid("RPC result envelope does not match its request."));
                }
                self.poisoned = false;
                Ok(result)
            }
        }
    }
}
/// Extract success while preserving a domain failure in an envelope.
pub fn success<T>(envelope: Envelope<T>) -> Result<T> {
    match envelope.outcome {
        Outcome::Success { result } => Ok(result),
        Outcome::Failure { error } => Err(error),
    }
}

#[derive(Deserialize)]
struct HandshakeResult {
    version: u32,
    project_id: Uuid,
    instance_id: Uuid,
}
