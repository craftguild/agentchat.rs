//! Low-level transaction and host-validation ports; business policy lives above.

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use crate::delivery::{MessageItem, MessageStream, WakeBatch, WakeItem};
use crate::project::ProjectChannelPolicy;
use crate::{Result, model::*, sequence::Sequence, time::Timestamp};

/// Durable successful mutation result and its semantic retry identity.
#[derive(Debug, Clone)]
pub struct IdempotencyRecord {
    /// Request UUID.
    pub request_id: Uuid,
    /// Exact stable host/conversation identity, independent of reconnect incarnation.
    pub actor_key: String,
    /// Typed application operation name.
    pub operation: String,
    /// Hash of normalized typed arguments.
    pub argument_hash: String,
    /// Original successful public result.
    pub result: Value,
    /// Original commit time.
    pub committed_at: Timestamp,
}

/// An ORM-independent serialized mutation transaction. Dropping it rolls back.
///
/// Event allocation is staged: aggregate foreign keys may reference the allocated
/// sequence, then publication fills the event references and durable deliveries.
/// `commit` must reject any allocated event that was never published. Staging
/// rows are never visible outside their uncommitted transaction.
#[async_trait]
pub trait Transaction: Send {
    /// Open one sequential nested savepoint, borrowing its immediate parent.
    /// Finish this child before using the parent. Child commit only releases
    /// the savepoint; only the outer commit establishes durable success.
    async fn savepoint(&mut self) -> Result<Box<dyn Transaction + '_>>;
    /// Explicitly roll back this transaction/savepoint before later work.
    async fn rollback(self: Box<Self>) -> Result<()>;
    /// Retrieve a successful key before checking current mutable business state.
    async fn idempotency(&mut self, request_id: Uuid) -> Result<Option<IdempotencyRecord>>;
    /// Save a successful result in the same transaction as its business effects.
    async fn save_idempotency(&mut self, record: &IdempotencyRecord) -> Result<()>;
    /// Find retained identity by UUID.
    async fn agent(&mut self, id: Uuid) -> Result<Option<Agent>>;
    /// Find exact host/conversation identity; never infer from a directory.
    async fn conversation(&mut self, host: HostKind, session_id: &str) -> Result<Option<Agent>>;
    /// Look up a retained handle, including left identities.
    async fn handle(&mut self, handle: &str) -> Result<Option<Agent>>;
    /// Snapshot all currently joined identities under the write boundary.
    async fn joined_agents(&mut self) -> Result<Vec<Agent>>;
    /// Insert/update an identity without removing retained history.
    async fn save_agent(&mut self, agent: &Agent) -> Result<()>;
    /// Common typed actor, including a nonreceiving anonymous user.
    async fn actor(&mut self, id: Uuid) -> Result<Option<Actor>>;
    /// Project-common anonymous identity, absent before its first Join.
    async fn user(&mut self) -> Result<Option<Actor>>;
    /// Save common participation without manufacturing native metadata.
    async fn save_actor(&mut self, actor: &Actor) -> Result<()>;
    /// Joined common participants for mandatory membership policy.
    async fn joined_actors(&mut self) -> Result<Vec<Actor>>;
    /// Latest launch for an identity, including its terminal generation.
    async fn latest_launch(&mut self, agent: Uuid) -> Result<Option<Launch>>;
    /// Exact retained generation, preventing replay after a newer launch.
    async fn launch(&mut self, generation: Uuid) -> Result<Option<Launch>>;
    /// At most 32 active generations after the supplied UUID, in UUID order.
    async fn active_launches(&mut self, after: Option<Uuid>) -> Result<Vec<Launch>>;
    /// Insert immutable launch facts or make its one-way retirement transition.
    async fn save_launch(&mut self, launch: &Launch) -> Result<()>;
    /// Retained current/last connection for an identity.
    async fn session(&mut self, agent: Uuid) -> Result<Option<Session>>;
    /// Disconnect previous incarnations and save this exact incarnation.
    async fn save_session(&mut self, session: &Session) -> Result<()>;
    /// Invalidate persisted connection liveness when its owning runtime stops.
    /// Participation, pending work and pause state remain retained.
    async fn disconnect_sessions(&mut self) -> Result<()>;
    /// Find a channel by normalized slug.
    async fn channel(&mut self, name: &str) -> Result<Option<Channel>>;
    /// List all retained channels in sequence order.
    async fn channels(&mut self) -> Result<Vec<Channel>>;
    /// Stored project channel policy, or none for a legacy unset project.
    async fn channel_policy(&mut self) -> Result<Option<ProjectChannelPolicy>>;
    /// Replace the complete nonempty ordered project channel policy.
    async fn save_channel_policy(&mut self, policy: &ProjectChannelPolicy) -> Result<()>;
    /// Current project receipt policy; input claims retain their own snapshot.
    async fn receipt_settings(&mut self) -> Result<crate::project::ProjectReceiptSettings>;
    /// Replace validated configuration without changing any original input claim.
    async fn save_receipt_settings(
        &mut self,
        settings: &crate::project::ProjectReceiptSettings,
    ) -> Result<()>;
    /// Insert a channel with a committed-event reference.
    async fn insert_channel(&mut self, channel: &Channel) -> Result<()>;
    /// Insert the supplied membership once; caller owns expansion policy.
    async fn add_member(&mut self, channel: Uuid, agent: Uuid, seq: Sequence) -> Result<()>;
    /// Whether one retained participant currently belongs to one channel.
    async fn is_member(&mut self, channel: Uuid, agent: Uuid) -> Result<bool>;
    /// Current membership's forward-delivery boundary, when present.
    async fn membership_boundary(&mut self, channel: Uuid, agent: Uuid)
    -> Result<Option<Sequence>>;
    /// Remove one optional-channel membership without changing participation.
    async fn remove_member(&mut self, channel: Uuid, agent: Uuid) -> Result<()>;
    /// Cancel not-yet-submitted work for one removed channel membership and
    /// retire any outstanding wake batch left with no active deliveries.
    async fn cancel_channel_deliveries(
        &mut self,
        channel: Uuid,
        agent: Uuid,
        at: Timestamp,
    ) -> Result<()>;
    /// Count current members of one retained channel.
    async fn channel_member_count(&mut self, channel: Uuid) -> Result<u64>;
    /// Current joined participants in one channel.
    async fn channel_members(&mut self, channel: Uuid) -> Result<Vec<Agent>>;
    /// Remove active memberships on explicit leave, preserving all identities.
    async fn remove_memberships(&mut self, agent: Uuid) -> Result<()>;
    /// Retrieve one immutable message.
    async fn message(&mut self, id: Uuid) -> Result<Option<Message>>;
    /// Fetch at most 200 distinct message IDs, omitting missing rows. Decode
    /// complete records; callers restore normalized order before any writes.
    async fn messages(&mut self, ids: &[Uuid]) -> Result<Vec<Message>>;
    /// Stage one immutable Header using the transactional project-order allocator.
    async fn allocate_dispatch(
        &mut self,
        kind: DispatchKind,
        channel: Option<Uuid>,
        now: Timestamp,
    ) -> Result<Dispatch>;
    /// Publish exactly one matching subtype, frozen recipients and initial
    /// histories atomically; an ignored failure cannot expose a partial aggregate.
    async fn publish_dispatch(
        &mut self,
        dispatch: &Dispatch,
        payload: &DispatchPayload,
        deliveries: &[Delivery],
    ) -> Result<()>;
    /// Read the immutable sending Header, independent of audit event lookup.
    async fn dispatch(&mut self, id: Uuid) -> Result<Option<Dispatch>>;
    /// Persist the complete frozen wake-recipient snapshot.
    async fn insert_mention(&mut self, mention: &Mention) -> Result<()>;
    /// Retrieve the sparse explicit receipt, if one exists.
    async fn receipt(&mut self, message: Uuid, agent: Uuid) -> Result<Option<Receipt>>;
    /// Existing first reads for one exact agent and at most 200 distinct
    /// message IDs, without creating missing receipts or changing their times.
    async fn receipts(&mut self, agent: Uuid, ids: &[Uuid]) -> Result<Vec<Receipt>>;
    /// Read one independently retained origin; automatic reads are not public input.
    async fn receipt_kind(
        &mut self,
        message: Uuid,
        actor: Uuid,
        kind: ReadKind,
    ) -> Result<Option<Receipt>>;
    /// Persist one first-read transition.
    async fn insert_receipt(&mut self, receipt: &Receipt) -> Result<()>;
    /// Insert at most 200 distinct message/agent first reads already validated
    /// in this immediate transaction. Conflicts must fail, never overwrite an
    /// earlier time or silently omit a row. Require the full inserted count.
    async fn insert_receipts(&mut self, receipts: &[Receipt]) -> Result<()>;
    /// At most 200 processing rows strictly past their immutable deadline.
    async fn due_deliveries(&mut self, now: Timestamp) -> Result<Vec<Delivery>>;
    /// Existing one-per-original-message timeout notification coalescing identity.
    async fn delivery_timeout_notice(&mut self, message: Uuid) -> Result<Option<Uuid>>;
    /// Save only the immutable Message-to-Notice Dispatch identity, never state.
    async fn save_delivery_timeout_notice(&mut self, message: Uuid, notice: Uuid) -> Result<()>;
    /// Count expired original recipient rows and sample at most twenty deliveries.
    async fn expired_recipients(&mut self, message: Uuid) -> Result<ExpiredRecipients>;
    /// Retain the explicit author's cancellation boundary through the current
    /// event head, including Leave after an already retired participation.
    async fn cancel_author_timeout_notices(&mut self, actor: Uuid) -> Result<()>;
    /// Whether this original message follows its author's last explicit Leave.
    /// Rejoining cannot revive a notification cancelled by that boundary.
    async fn author_timeout_notice_allowed(
        &mut self,
        actor: Uuid,
        created: Sequence,
    ) -> Result<bool>;
    /// Retrieve one exact immutable audit occurrence; it creates no delivery work.
    async fn event(&mut self, id: Uuid) -> Result<Option<Event>>;
    /// Find one stable delivery for full-set ownership validation.
    async fn delivery(&mut self, id: Uuid) -> Result<Option<Delivery>>;
    /// Fetch at most 200 distinct delivery IDs in this immediate transaction.
    /// Missing IDs are omitted; the application validates the complete requested
    /// set in its original normalized order before making any transition.
    async fn deliveries(&mut self, ids: &[Uuid]) -> Result<Vec<Delivery>>;
    /// Persist delivery bookkeeping, never creating a receipt.
    async fn save_delivery(&mut self, delivery: &Delivery) -> Result<()>;
    /// End pending/processing scope work while retaining first-time history.
    async fn cancel_deliveries(&mut self, agent: Uuid, at: Timestamp) -> Result<()>;
    /// Exact launch settlement cursor and original-result uncertainty head.
    async fn message_stream(&mut self, generation: Uuid) -> Result<Option<MessageStream>>;
    /// Persist one cursor/head transition without changing read/presence facts.
    async fn save_message_stream(&mut self, stream: &MessageStream) -> Result<()>;
    /// Return a complete message only when it is the earliest eligible native
    /// input after the settlement cursor. An earlier notice cannot be skipped.
    async fn next_message(&mut self, agent: Uuid, after: Sequence) -> Result<Option<MessageItem>>;
    /// Earliest eligible sealed non-echo native input, including typed notices.
    /// Unsupported adapters refuse rather than silently skipping input kinds.
    async fn next_native(
        &mut self,
        _agent: Uuid,
        _after: Sequence,
    ) -> Result<Option<crate::delivery::NativeItem>> {
        Err(crate::Error::new(
            crate::ErrorCode::UnsupportedProtocol,
            "Ordered native event lookup is unavailable.",
        ))
    }
    /// Reconstruct one exact retained stream head for completion or receipt
    /// recovery. This never selects or authorizes a new native submission.
    async fn retained_native(
        &mut self,
        agent: Uuid,
        delivery: Uuid,
        seq: Sequence,
    ) -> Result<Option<crate::delivery::NativeItem>>;
    /// One retained wake batch by exact identity.
    async fn wake_batch(&mut self, id: Uuid) -> Result<Option<WakeBatch>>;
    /// The unique outstanding batch for this recipient/incarnation.
    async fn outstanding_wake(
        &mut self,
        agent: Uuid,
        incarnation: Uuid,
    ) -> Result<Option<WakeBatch>>;
    /// Insert/update control fields, preserving batch owner and incarnation.
    async fn save_wake_batch(&mut self, batch: &WakeBatch) -> Result<()>;
    /// Whether this exact recipient/batch has pending or inflight deliveries
    /// referencing sealed events. Validate bounded delivery rows and reject
    /// more than twenty, without fetching/deserializing their event payloads.
    async fn wake_batch_has_pending(&mut self, agent: Uuid, batch: Uuid) -> Result<bool>;
    /// At most twenty pending/processing targeted rows, in event order. None selects
    /// unassigned work; Some selects only that batch's already assigned rows.
    async fn wake_items(&mut self, agent: Uuid, batch: Option<Uuid>) -> Result<Vec<WakeItem>>;
    /// Number of pending/inflight recipient rows excluding its own actor echoes.
    async fn pending_delivery_count(&mut self, agent: Uuid) -> Result<u64>;
    /// Retire other incarnations' outstanding batches and requeue their unacked
    /// deliveries without changing event/delivery IDs or cumulative attempts.
    async fn retire_wake_incarnations(&mut self, agent: Uuid, current: Uuid) -> Result<()>;
    /// Allocate an unpublished event skeleton under this transaction.
    async fn allocate_event(&mut self, kind: EventKind, now: Timestamp) -> Result<Event>;
    /// Seal event references and all explicit recipient rows together.
    async fn publish_event(&mut self, event: &Event) -> Result<()>;
    /// Find a retained lease group including ordered patterns.
    async fn reservation(&mut self, id: Uuid) -> Result<Option<Reservation>>;
    /// Up to 200 active groups after an immutable UUID, in ascending UUID order.
    /// Deadline/state decisions belong to the application; page scans retain the
    /// immediate transaction while they change state or decide exclusivity.
    async fn active_reservations(&mut self, after: Option<Uuid>) -> Result<Vec<Reservation>>;
    /// Insert/update a group while preserving immutable ownership/generation.
    async fn save_reservation(&mut self, reservation: &Reservation) -> Result<()>;
    /// Commit all effects, rejecting unpublished events and incomplete state.
    async fn commit(self: Box<Self>) -> Result<()>;
}

/// Storage adapter starts a serialized write transaction before business reads.
#[async_trait]
pub trait Storage: Send + Sync {
    /// Persistent namespace owned by this adapter, independent of a path.
    fn project_id(&self) -> Uuid;
    /// Begin immediate, not a deferred read-then-write transaction.
    async fn begin(&self) -> Result<Box<dyn Transaction>>;
    /// Begin an isolated read-only snapshot with a captured committed head.
    async fn snapshot(&self) -> Result<Box<dyn crate::read::Snapshot>>;
    /// Conservative read-only hint for at most 32 exact conversations. Include
    /// any outstanding batch or unassigned sealed pending/inflight targeted
    /// work. Only a same-incarnation batch with validated nonempty pending rows
    /// may be omitted using `WakeBatch::retry_deferred(now)`. Empty assigned
    /// sets and due exhausted attempts must reach ordinary reconciliation.
    /// Use one fresh read snapshot, never authorize presentation/refresh presence
    /// or cache negatives. Returning all inputs is valid; errors need full scan.
    async fn wake_candidates(&self, contexts: &[Context], _now: Timestamp) -> Result<Vec<Uuid>> {
        if contexts.len() > crate::delivery::MAX_WAKE_SCAN_CONTEXTS {
            return Err(crate::Error::new(
                crate::ErrorCode::InvalidArgument,
                "Wake candidate scan exceeds 32 contexts.",
            ));
        }
        Ok(contexts.iter().map(|context| context.context_id).collect())
    }
}

/// Host adapter eligibility and genuine root-conversation liveness proof.
#[async_trait]
pub trait HostGate: Send + Sync {
    /// Trusted mechanism configuration only, captured outside write transactions.
    /// This must not depend on model/tool arguments or grant output authority.
    /// A changed declaration cannot reinterpret an unresolved persisted head.
    fn input_capabilities(&self, context: &Context) -> Result<crate::delivery::InputCapabilities>;
    /// Validate the exact host/OS profile and context without changing policies.
    async fn verify(&self, context: &Context) -> Result<HostState>;
    /// Resolve AGENTCHAT-owned startup authority for this exact host root. Ordinary
    /// native startup/MCP metadata must return none. This never creates authority.
    /// Read bounded local binding facts only; no native/application callback or
    /// writer lock, including when called by the independent final-hook snapshot.
    async fn launch_identity(&self, _context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(None)
    }
    /// Resolve output authority for this exact host root after any provider-side
    /// admission response has become externally visible. The default shares the
    /// startup identity; adapters with a held transition response may withhold
    /// delivery without weakening Join or managed-activation verification.
    async fn launch_delivery_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        self.launch_identity(context).await
    }
    /// Optional immutable launcher-selected name for the first explicit Join.
    /// Read bounded local launch facts only; retained conversation handles and
    /// explicit names take precedence in shared policy. This grants no Join.
    async fn launch_name(&self, _context: &Context) -> Result<Option<String>> {
        Ok(None)
    }
    /// Bounded synchronous kernel-only check; no native/application callbacks,
    /// waits, heartbeats or outbound writes. Unknown suspends, never retires.
    fn launch_owner(&self, _identity: &LaunchIdentity) -> LaunchOwnerState {
        LaunchOwnerState::Unknown
    }
    /// Read the broad root fence used to suspend delivery during a transition.
    /// Adapters may return true for reversible provisional state as well as a
    /// committed departure; irreversible decisions use `launch_root_departed`.
    /// False includes absence/uncertainty and never grants output authority.
    fn launch_root_changed(
        &self,
        _project: Uuid,
        _host: HostKind,
        _root: &str,
        _identity: &LaunchIdentity,
    ) -> bool {
        false
    }
    /// Read the committed positive departure required to authorize managed
    /// activation. Adapters whose broad delivery fence also includes provisional
    /// transition state must override this with their durable departure record.
    fn launch_root_departed(
        &self,
        project: Uuid,
        host: HostKind,
        root: &str,
        identity: &LaunchIdentity,
    ) -> bool {
        self.launch_root_changed(project, host, root, identity)
    }
    /// Explicit isolated human/synthetic-client path. Production native gates
    /// retain false regardless of best-effort host compatibility.
    fn allows_unmanaged_participation(&self) -> bool {
        false
    }
}

/// Verified phase snapshot from the real host adapter, never guessed at join.
#[derive(Debug, Clone)]
pub struct HostState {
    /// Current host phase, including approval/interruption controls.
    pub phase: Phase,
    /// Expected active turn ID for safe steering, when available.
    pub active_turn_id: Option<String>,
    /// Pause/interruption latch maintained by authoritative host lifecycle input.
    pub wake_paused: bool,
}
