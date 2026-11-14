//! Native-owned observation transport; it grants no root, presence or wake proof.
mod files;
mod identity;
mod ownership;
mod protocol;
mod publication;
mod recovery;
mod refresh;
mod stdio;
pub(super) use protocol::Endpoint;
pub use protocol::Question;
pub(crate) use refresh::request_context_refresh;
pub use stdio::{native_output, read_answer, read_hook_input, read_input};
pub(super) fn allocation_publication(
    root: &identity::Directory,
    original_path: &Path,
    name: &std::ffi::OsStr,
    snapshot: &identity::Snapshot,
) -> Result<crate::staging::allocation::Published> {
    let value: Endpoint = serde_json::from_slice(&snapshot.bytes).map_err(|_| unavailable())?;
    let proof = value.ownership.as_ref().ok_or_else(unavailable)?;
    proof.validate_original(&value, root.id, snapshot.id, original_path)?;
    if name != std::ffi::OsStr::new(&format!("fresh-{}.json", value.observer))
        || !super::identifier(&value.observer)
        || value.owner.is_none()
    {
        return Err(unavailable());
    }
    Ok(crate::staging::allocation::Published {
        path: value.socket.parent().ok_or_else(unavailable)?.into(),
        helper: proof.relay.clone(),
        native: value.owner.clone().ok_or_else(unavailable)?,
        directory: proof.directory,
        parent: proof.parent,
        sockets: vec![
            ("request.sock".into(), proof.request.clone()),
            ("reply.sock".into(), proof.reply.clone()),
        ],
    })
}

use crate::claude::{
    freshness::Snapshot,
    namespace::OwnerNamespace,
    process::{BirthState, ProcessIdentity},
};
use agentchat_core::{Error, ErrorCode, Result};
use protocol::{Receipt, Request};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    net::UnixStream,
    time::{Instant, timeout, timeout_at},
};
use uuid::Uuid;

/// Serve one physical native observer through bounded private sockets/stdout.
/// The supplied owner must be independently pinned by the native entrypoint.
/// A fresh native API answer is still required for every request. Replacement
/// retires the preceding same-owner relay without killing a process. Owner loss,
/// output failure or cancellation releases only this publication's own inodes.
/// No project/context, heartbeat, application policy or model prompt is created.
pub async fn serve<W: AsyncWrite + Unpin>(
    root: &Path,
    observer: &str,
    owner: ProcessIdentity,
    output: W,
) -> Result<()> {
    serve_inner(root, observer, owner, output, None).await
}

/// Observe native state and emit a bounded metadata-renewal hint when the owned
/// MCP endpoint changes. The native module must capture its actual root and
/// register fresh facts. This performs no application operation or model wake.
/// A separate original-supervisor request can also ask for context renewal;
/// ordinary observation questions remain read-only and cannot issue that hint.
pub async fn serve_managed<W: AsyncWrite + Unpin>(
    namespace: OwnerNamespace,
    output: W,
) -> Result<()> {
    let root = namespace.directory().to_owned();
    let observer = namespace.observer().to_owned();
    let owner = namespace.owner().clone();
    serve_inner(&root, &observer, owner, output, Some(namespace)).await
}
async fn serve_inner<W: AsyncWrite + Unpin>(
    root: &Path,
    observer: &str,
    owner: ProcessIdentity,
    mut output: W,
    namespace: Option<OwnerNamespace>,
) -> Result<()> {
    let publication = if let Some(namespace) = &namespace {
        publication::bind_managed(namespace).await?
    } else {
        files::bind(root, observer, &owner).await?
    };
    let mut ticks = tokio::time::interval(Duration::from_millis(250));
    let mut sequence = 0u64;
    let mut endpoint_nonce = None;
    let mut watched = None::<(Uuid, ProcessIdentity)>;
    let mut reported_loss = None;
    loop {
        let mut peer = tokio::select! {
            result=publication.requests.accept()=>result.map_err(|_|unavailable())?.0,
            _=ticks.tick()=> {
                match owner.continuity_state() {
                    crate::claude::process::ContinuityState::Gone => return Ok(()),
                    crate::claude::process::ContinuityState::Unknown => continue,
                    crate::claude::process::ContinuityState::Current => {}
                }
                if let Some(namespace)=&namespace {
                    let endpoint=namespace.watched_transport().await?;
                    if let Some((endpoint, helper))=&endpoint {
                        // An unproved/changed publication invalidates cached
                        // proof. Mere absence never establishes helper death.
                        watched=helper.clone().map(|h|(endpoint.nonce,h));
                    }
                    let current=endpoint.map(|(e,_)|e.nonce);
                    if current != endpoint_nonce {
                        endpoint_nonce=current;
                        if let Some(nonce)=current {
                            let notice=serde_json::json!({"event":"transport_changed","endpoint_nonce":nonce});
                            timeout(Duration::from_secs(2),protocol::write(&mut output,&notice)).await
                                .map_err(|_|output_lost())?.map_err(|_|output_lost())?;
                        }
                    }
                    if let Some((nonce, helper))=&watched
                        && reported_loss != Some(*nonce)
                        && helper.birth_state()==BirthState::Gone {
                        let notice=serde_json::json!({"event":"transport_lost","endpoint_nonce":nonce});
                        timeout(Duration::from_secs(2),protocol::write(&mut output,&notice)).await
                            .map_err(|_|output_lost())?.map_err(|_|output_lost())?;
                        reported_loss=Some(*nonce);
                    }
                }
                continue;
            }
        };
        if !files::same_user(&peer) {
            continue;
        }
        let outcome = timeout(Duration::from_secs(5), async {
            match protocol::read(&mut peer, 1024).await? {
                Request::RefreshContext(request) => {
                    let accepted = if let Some(namespace) = &namespace {
                        refresh::emit(namespace, &peer, &request, &mut output).await?
                    } else {
                        false
                    };
                    protocol::write(
                        &mut peer,
                        &Receipt {
                            nonce: request.refresh_context,
                            accepted,
                        },
                    )
                    .await?;
                    Ok(false)
                }
                Request::Retire(request) => {
                    let accepted = request.retire == publication.token;
                    protocol::write(
                        &mut peer,
                        &Receipt {
                            nonce: request.retire,
                            accepted,
                        },
                    )
                    .await?;
                    Ok(accepted)
                }
                Request::Observe(request) => {
                    if !owner.is_continuing() || request.nonce.is_nil() {
                        return Err(unavailable());
                    }
                    sequence = sequence.checked_add(1).ok_or_else(unavailable)?;
                    let question = Question {
                        nonce: request.nonce,
                        relay_sequence: sequence,
                        reply_socket: publication.reply_socket.clone(),
                    };
                    // Only stdout failure terminates the native stream. An old
                    // cancelled client must not tear down an independent relay.
                    output
                        .write_all(&protocol::encode(&question)?)
                        .await
                        .map_err(|_| output_lost())?;
                    output.flush().await.map_err(|_| output_lost())?;
                    let deadline = Instant::now() + Duration::from_secs(4);
                    for _ in 0..32 {
                        let mut reply = timeout_at(deadline, publication.replies.accept())
                            .await
                            .map_err(|_| unavailable())?
                            .map_err(|_| unavailable())?
                            .0;
                        if !files::same_user(&reply) {
                            continue;
                        }
                        let answer: Snapshot =
                            match timeout_at(deadline, protocol::read(&mut reply, 8192)).await {
                                Ok(Ok(answer)) => answer,
                                _ => continue,
                            };
                        let accepted = owner.is_continuing()
                            && protocol::answer_matches(&answer, &question, observer);
                        let _ = protocol::write(
                            &mut reply,
                            &Receipt {
                                nonce: answer.nonce,
                                accepted,
                            },
                        )
                        .await;
                        if accepted {
                            protocol::write(&mut peer, &answer).await?;
                            return Ok(false);
                        }
                    }
                    Err(unavailable())
                }
            }
        })
        .await;
        match outcome {
            Err(_) => return Err(output_lost()),
            Ok(Ok(true)) => return Ok(()),
            Ok(Err(error)) if error.code == ErrorCode::IoError => return Err(error),
            _ => {}
        }
    }
}

/// Send one bounded native API answer and require its exact transport receipt.
/// Late/wrong nonces cannot complete another request. This receipt is internal
/// transport bookkeeping, never an application delivery ACK or read receipt.
pub async fn respond(socket: &Path, answer: &Snapshot) -> Result<()> {
    files::socket(socket).await?;
    timeout(Duration::from_secs(2), async {
        let mut stream = UnixStream::connect(socket)
            .await
            .map_err(|_| unavailable())?;
        if !files::same_user(&stream) {
            return Err(unavailable());
        }
        protocol::write(&mut stream, answer).await?;
        let receipt: Receipt = protocol::read(&mut stream, 1024).await?;
        if receipt.nonce != answer.nonce || !receipt.accepted {
            return Err(unavailable());
        }
        Ok(())
    })
    .await
    .map_err(|_| unavailable())?
}

/// Retire only this physical owner's named transport, without process signals.
/// A predecessor cannot stop a different live successor. This releases only
/// observation I/O; application participation and lifecycle retirement remain
/// the host adapter's separate obligations.
pub async fn retire(root: &Path, observer: &str, owner: &ProcessIdentity) -> Result<()> {
    super::files::directory(root).await?;
    if !super::identifier(observer) {
        return Err(unavailable());
    }
    timeout(
        Duration::from_secs(6),
        files::retire(
            &root.join(format!("fresh-{observer}.json")),
            observer,
            owner,
        ),
    )
    .await
    .map_err(|_| unavailable())?
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The native observation transport is unavailable or obsolete.",
    )
}
fn output_lost() -> Error {
    Error::new(
        ErrorCode::IoError,
        "The native observation stream has ended.",
    )
}
