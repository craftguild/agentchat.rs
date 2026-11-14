//! Typed in-process native writes share the existing bounded RPC ingress.
use super::Answer;
use agentchat_core::{
    Result,
    application::{CommitOutcome, CommitRequest, NativeClaim, NativeProcessing},
    delivery::{MessageProgress, NativeCompletion},
    model::Context,
};
use agentchat_protocol::rpc::Call;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub(super) enum Command {
    Call {
        request_id: Option<Uuid>,
        queued_at: Instant,
        connection: Uuid,
        call: Box<Call>,
        bound: Option<Uuid>,
        reply: oneshot::Sender<Result<Answer>>,
    },
    Native(Box<NativePending>),
    Gone(Uuid),
    Stop(oneshot::Sender<Result<()>>),
}
pub(crate) struct Queue {
    sender: mpsc::Sender<Command>,
    receiver: mpsc::Receiver<Command>,
}
impl Queue {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::channel(64);
        Self { sender, receiver }
    }
    pub(crate) fn native(&self) -> NativeIngress {
        NativeIngress {
            sender: self.sender.clone(),
        }
    }
    pub(super) fn into_parts(self) -> (mpsc::Sender<Command>, mpsc::Receiver<Command>) {
        (self.sender, self.receiver)
    }
}
#[derive(Clone)]
pub(crate) struct NativeIngress {
    sender: mpsc::Sender<Command>,
}
impl NativeIngress {
    pub(crate) async fn write(
        &self,
        request: NativeRequest,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<CommitOutcome> {
        let (reply, receive) = oneshot::channel();
        let command = Command::Native(Box::new(NativePending {
            request,
            reply,
            deadline,
        }));
        // The caller already owns one bounded native job (or one existing test
        // consumer). Waiting retains its result, not a new task or side queue.
        self.sender
            .send(command)
            .await
            .map_err(|_| super::super::unavailable("Runtime native ingress stopped."))?;
        receive
            .await
            .map_err(|_| super::super::unavailable("Runtime native write result was lost."))?
    }
}
pub(crate) enum NativeRequest {
    Processing(NativeProcessing),
    Claim(NativeClaim),
    Completion(Box<NativeCompletion>),
}
impl NativeRequest {
    pub fn context(&self) -> &Context {
        match self {
            Self::Processing(r) => r.context(),
            Self::Claim(r) => r.context(),
            Self::Completion(r) => &r.context,
        }
    }
    pub fn preparatory(&self) -> bool {
        !matches!(self, Self::Completion(_))
    }
    pub fn into_commit(self) -> CommitRequest {
        match self {
            Self::Processing(r) => CommitRequest::NativeProcessing(r),
            Self::Claim(r) => CommitRequest::NativeClaim(r),
            Self::Completion(r) => CommitRequest::NativeCompletion(*r),
        }
    }
    pub fn accepted_input(&self) -> Option<(Uuid, agentchat_core::sequence::Sequence, bool)> {
        match self {
            Self::Completion(c) if matches!(&c.outcome, Ok(MessageProgress::Accepted(_))) => {
                Some((
                    c.dispatch.delivery_id(),
                    c.dispatch.seq(),
                    c.dispatch.awaiting_acceptance(),
                ))
            }
            _ => None,
        }
    }
}
pub(crate) struct NativePending {
    pub request: NativeRequest,
    pub reply: oneshot::Sender<Result<CommitOutcome>>,
    pub deadline: Option<tokio::time::Instant>,
}
impl NativePending {
    pub fn skip_preparation(&self) -> bool {
        self.request.preparatory()
            && (self.reply.is_closed()
                || self
                    .deadline
                    .is_some_and(|v| tokio::time::Instant::now() >= v))
    }
    pub fn stream_key(&self) -> (Uuid, String, String) {
        let c = self.request.context();
        (
            c.project_id,
            c.host.as_str().into(),
            c.host_session_id.clone(),
        )
    }
}

/// Adjacent ready requests retain independent typed reply channels.
pub(super) enum Pending {
    Mutation(MutationPending),
    Native(NativePending),
}
pub(super) struct MutationPending {
    pub request_id: Option<Uuid>,
    pub queued_at: Instant,
    pub call: Call,
    pub bound: Option<Uuid>,
    pub reply: oneshot::Sender<Result<Answer>>,
}
pub(super) fn collect(
    first: Option<Pending>,
    receiver: &mut mpsc::Receiver<Command>,
    carried: &mut Option<Command>,
    limit: usize,
) -> Vec<Pending> {
    let mut pending = first.into_iter().collect::<Vec<_>>();
    // A carried Query/control/lifecycle fence precedes the entire queue.
    if carried.is_some() {
        return pending;
    }
    let mut streams = pending
        .iter()
        .filter_map(|p| match p {
            Pending::Native(n) => Some(n.stream_key()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    while pending.len() < limit {
        match receiver.try_recv() {
            Ok(Command::Call {
                call,
                bound,
                reply,
                request_id,
                queued_at,
                ..
            }) if call.grouped_mutation() => {
                pending.push(Pending::Mutation(MutationPending {
                    call: *call,
                    bound,
                    reply,
                    request_id,
                    queued_at,
                }));
            }
            Ok(Command::Native(native)) => {
                if !streams.insert(native.stream_key()) {
                    *carried = Some(Command::Native(native));
                    break;
                }
                pending.push(Pending::Native(*native));
            }
            Ok(other) => {
                *carried = Some(other);
                break;
            }
            Err(_) => break,
        }
    }
    pending
}

#[cfg(test)]
#[path = "../../../tests/unit/native_ingress.rs"]
mod tests;
