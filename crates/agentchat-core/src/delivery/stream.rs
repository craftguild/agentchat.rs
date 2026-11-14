//! Durable full-body handoff ordering is independent of message reading.
use super::{HostReceipt, MessageInput, Presentation};
use crate::{Result, model::*, sequence::Sequence, time::Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One cursor for one immutable native launch, shared by all native body routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageStream {
    /// Launch authority; reconnecting cannot replace this generation.
    pub generation: Uuid,
    /// Startup boundary or last head settled by a correlated native result.
    pub settled_seq: Sequence,
    /// Failed, ambiguous or currently submitting head; later bodies cannot pass.
    pub head: Option<MessageHead>,
    /// Terminal cancellation preserves the last cursor and head for diagnosis.
    pub cancelled: bool,
}
impl MessageStream {
    /// Start after the exact committed join boundary with no historical replay.
    pub fn new(launch: &Launch) -> Self {
        Self {
            generation: launch.identity.generation,
            settled_seq: launch.start_seq,
            head: None,
            cancelled: launch.retired_at.is_some(),
        }
    }
}
/// One stable message selected before native I/O, with a replaceable attempt fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHead {
    /// Stable original recipient/event row.
    pub delivery_id: Uuid,
    /// Committed project sequence, strictly after the accepted cursor.
    pub seq: Sequence,
    /// Current application connection at the time this attempt was claimed.
    pub incarnation: Uuid,
    /// Fresh callback fence; an orphan or previous incarnation cannot complete it.
    pub token: Uuid,
    /// Cumulative head attempts across reconnects, bounded to three.
    pub attempts: u32,
    /// Submission/orphan deadline, safe retry time or next pending-receipt poll.
    pub retry_at: Timestamp,
    /// The adapter claims its original receipt lane before any publication I/O.
    /// Presence allows only result recovery; it does not prove file/native entry.
    /// Polling this same attempt is not another submission and cannot advance FIFO.
    pub awaiting_acceptance: bool,
    /// This attempt may have reached a native host without a durable receipt
    /// lane. Persist before I/O; a timeout/orphan cannot authorize another send.
    pub submission_pending: bool,
    /// Effective policy captured by the original input claim.
    pub receipt_policy: crate::project::InputReceiptPolicy,
    /// Exact original frame contract for this fresh-format claim.
    pub frame_revision: crate::project::NativeFrameRevision,
    /// Bounded machine failure code; never native messages or original bodies.
    pub failure_code: Option<String>,
}
impl MessageHead {
    /// Check the original unresolved mechanism without changing any retained
    /// token, retry time or uncertainty. The two historical flags already encode
    /// the recovery contract; a resolved refusal has neither flag and may retry.
    pub fn require_recovery(&self, recovery: super::InputRecovery) -> Result<()> {
        let matches = match (self.awaiting_acceptance, self.submission_pending) {
            (true, false) => recovery == super::InputRecovery::OriginalReceipt,
            (false, true) => recovery == super::InputRecovery::RetainUncertainty,
            (false, false) => true,
            (true, true) => false,
        };
        if matches {
            Ok(())
        } else {
            Err(crate::Error::new(
                crate::ErrorCode::UnsupportedProtocol,
                "The adapter cannot recover the original unresolved input mechanism.",
            ))
        }
    }
    /// An unresolved submission is visibly uncertain after its bounded request
    /// window or a reported failure. It retains its original token and budget.
    pub fn acceptance_unknown(&self, now: Timestamp) -> bool {
        self.submission_pending && (self.failure_code.is_some() || now >= self.retry_at)
    }
}
/// Transaction-local complete original data selected in committed event order.
#[derive(Debug, Clone)]
pub struct MessageItem {
    /// Exact frozen recipient row, including its resolved mention decision.
    pub delivery: Delivery,
    /// Sealed Message sending Header, independent of audit publication.
    pub dispatch: Dispatch,
    /// Complete immutable original body.
    pub message: Message,
    /// Retained immutable sender identity.
    pub author: Actor,
    /// Retained immutable channel identity.
    pub channel: Channel,
}
/// A durable claim returned only after FULL commit, before external I/O.
#[derive(Debug, Clone)]
pub struct MessageDispatch {
    /// Complete attributed message; retries preserve every identity and body.
    pub input: MessageInput,
    /// Current unique submission fence.
    pub token: Uuid,
    /// Monotonic head attempt, never reset by reconnect or a new API route.
    pub attempt: u32,
    /// Fresh native phase used during preparation; adapters recheck before output.
    /// Ignored for receipt-only awaiting dispatches, which authorize no output.
    pub presentation: Presentation,
    /// Poll the existing publication instead of publishing or signalling again.
    pub awaiting_acceptance: bool,
    /// Original frame contract, retained unchanged across retries.
    pub frame_revision: crate::project::NativeFrameRevision,
}
/// Progress of a full-body native handoff; publication alone is not acceptance.
#[derive(Debug, Clone)]
pub enum MessageProgress {
    /// Positive proof that this attempt was not submitted or was explicitly
    /// rejected before acceptance. Only this result can permit a bounded retry
    /// of a fenced native request; an ordinary error carries no such guarantee.
    NotAccepted(crate::Error),
    /// The body is durably published locally, with no native acceptance yet.
    /// Retain the original head/token and retry budget while checking its receipt.
    AwaitingAcceptance,
    /// Explicit successful complete input acceptance, including a native queue.
    Accepted(HostReceipt),
}
/// One completed native body attempt, fenced against its original generation.
#[derive(Debug, Clone)]
pub struct MessageCompletion {
    /// Exact original application connection.
    pub context: Context,
    /// Previously committed full-body head/token.
    pub dispatch: MessageDispatch,
    /// One bounded I/O result; local publication is not native acceptance.
    pub outcome: Result<MessageProgress>,
}
/// Native full-body transport. One runtime job owns the current stream head.
#[async_trait]
pub trait MessagePort: Send + Sync {
    /// Present a non-message notice on the same application-owned native head.
    /// The default refuses rather than dropping events or treating them as
    /// message bodies. Ordinary native composition requires an implementation.
    async fn present_notice(
        &self,
        _context: &Context,
        _dispatch: &super::NoticeDispatch,
    ) -> Result<MessageProgress> {
        Err(crate::Error::new(
            crate::ErrorCode::UnsupportedProtocol,
            "This native port does not support ordered event context.",
        ))
    }
    /// Recheck exact root, owner and host controls, then hand off the full body.
    /// Return Accepted only for explicit native input acceptance in that exact
    /// conversation/generation; a host-internal queue counts. File-save/hook
    /// success, rejection, error or ambiguity cannot produce a success receipt.
    /// Do not await model processing, response, explicit reading or task acceptance.
    /// A later control change alone cannot downgrade an already confirmed success.
    /// Report NotAccepted only with definite non-submission/refusal evidence.
    /// Timeout, lost replies and cancellation do not supply that evidence.
    /// AwaitingAcceptance records only durable local publication. When the
    /// dispatch is already awaiting, poll its receipt without republishing or
    /// signalling that body again; keep the original head/token and attempt.
    /// That poll requires no new native phase proof and may run while paused or
    /// approval-blocked. It can only read an earlier result, never emit input.
    /// The application settles delivery and applies frozen read policy with verified acceptance.
    /// This port never writes receipts, renews authority or implies model action.
    async fn present_message(
        &self,
        context: &Context,
        dispatch: &MessageDispatch,
    ) -> Result<MessageProgress>;
}
