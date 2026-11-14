//! Bounded activation exchange, distinct from all native chat/API traffic.
use super::{Operations, Route, required};
use agentchat_core::{Result, application::Mutation, model::Context};
use agentchat_hosts::launch::{LaunchOwners, ReadyAuthorization, Registry};
use agentchat_protocol::{
    framing,
    rpc::{Call, success},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    time::Duration,
};
use tokio::{io::BufReader, net::UnixStream};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub version: u32,
    pub id: Uuid,
    pub context: Uuid,
    pub command: Mutation,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reply {
    version: u32,
    id: Uuid,
    result: Result<Value>,
}
impl Operations {
    pub(in crate::operations) async fn explicit_join(
        &self,
        route: &Route,
        id: Uuid,
        command: Mutation,
    ) -> Result<Value> {
        let context = route.context.as_ref().ok_or_else(|| required(None))?;
        let registry = Registry::new(self.manager.home());
        let Some(ready) = registry.authorization(context)? else {
            // Core alone decides the explicit isolated human/synthetic path.
            // Production AdmittedHosts refuses missing managed authorization;
            // neither public transport can enable this alternative with flags.
            let mut client = self.connect(route).await?;
            return success(
                client
                    .call(
                        id,
                        Call::Mutate {
                            request_id: id,
                            actor: agentchat_protocol::rpc::ActorReference::Native {
                                context_id: context.context_id,
                            },
                            command,
                        },
                    )
                    .await?,
            );
        };
        tokio::time::timeout(
            Duration::from_secs(35),
            exchange(
                &ready,
                Request {
                    version: 1,
                    id,
                    context: context.context_id,
                    command,
                },
            ),
        )
        .await
        .map_err(|_| unavailable())?
    }
}
async fn exchange(ready: &ReadyAuthorization, request: Request) -> Result<Value> {
    let context = &ready.binding().context;
    let path = ready.control();
    let uid = rustix::process::geteuid().as_raw();
    let directory =
        std::fs::symlink_metadata(path.parent().ok_or_else(|| required(Some(context)))?)
            .map_err(|_| required(Some(context)))?;
    let socket = std::fs::symlink_metadata(path).map_err(|_| required(Some(context)))?;
    if !directory.is_dir()
        || directory.file_type().is_symlink()
        || directory.uid() != uid
        || directory.permissions().mode() & 0o777 != 0o700
        || !socket.file_type().is_socket()
        || socket.uid() != uid
        || socket.permissions().mode() & 0o777 != 0o600
        || LaunchOwners::observe(&ready.binding().identity)
            != agentchat_core::model::LaunchOwnerState::Current
    {
        return Err(required(Some(context)));
    }
    let mut stream = UnixStream::connect(path)
        .await
        .map_err(|_| required(Some(context)))?;
    let credentials = stream.peer_cred().map_err(|_| required(Some(context)))?;
    if credentials.uid() != uid
        || !credentials
            .pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(|pid| ready.supervisor_peer(pid))
    {
        return Err(required(Some(context)));
    }
    let current = std::fs::symlink_metadata(path).map_err(|_| required(Some(context)))?;
    if current.dev() != socket.dev() || current.ino() != socket.ino() {
        return Err(required(Some(context)));
    }
    framing::write(&mut stream, &request).await?;
    let reply: Reply = framing::read(&mut BufReader::new(stream))
        .await?
        .ok_or_else(|| required(Some(context)))?;
    if reply.version != 1 || reply.id != request.id {
        return Err(required(Some(context)));
    }
    reply.result
}
pub(super) async fn receive(stream: &mut UnixStream) -> Result<Request> {
    // A control client cannot retain the sole original supervisor with an
    // unfinished request. Every accepted stream owns exactly one finite frame.
    tokio::time::timeout(
        Duration::from_secs(3),
        framing::read(&mut BufReader::new(stream)),
    )
    .await
    .map_err(|_| required(None))??
    .ok_or_else(|| required(None))
}
pub(super) async fn respond(stream: &mut UnixStream, id: Uuid, result: Result<Value>) {
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        framing::write(
            stream,
            &Reply {
                version: 1,
                id,
                result,
            },
        ),
    )
    .await;
}
pub(super) fn same_scope(a: &Context, b: &Context) -> bool {
    a.project_id == b.project_id
        && a.host == b.host
        && a.host_session_id == b.host_session_id
        && a.host_version == b.host_version
        && a.os_profile == b.os_profile
        && a.working_directory == b.working_directory
}

fn unavailable() -> agentchat_core::Error {
    let mut error = agentchat_core::Error::new(
        agentchat_core::ErrorCode::RuntimeUnavailable,
        "Managed activation result is unknown; retry with the original Join request_id.",
    );
    error.retryable = true;
    error
}
