//! Read-only snapshot ports, stable ordering and bounded public query shapes.

mod actor;
mod binding;
mod page;
mod presentation;
mod query;
mod receipts;
pub mod requests;
mod reservation;
mod result;
mod view;
mod viewer;

pub use crate::model::IdentityLabel;
pub use actor::*;
pub use binding::*;
pub(crate) use page::PageBuilder;
pub use page::{MAX_PAGE_BYTES, Page, PageRequest};
pub use query::{MessageFilter, Query};
pub use receipts::*;
pub use requests::ReadRequest;
pub use reservation::ReservationView;
pub use result::QueryResult;
pub use view::*;
pub use viewer::*;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    Result,
    model::*,
    sequence::{Cursor, Sequence},
    time::Timestamp,
};

/// Domain record with an immutable event-sequence pagination key.
#[derive(Debug, Clone)]
pub struct Indexed<T> {
    /// Stable ordering key; never a timestamp or mutable participation boundary.
    pub key: Sequence,
    /// Record at the captured snapshot.
    pub item: T,
}

/// A read-only database snapshot, anchored by its captured committed event head.
/// No method can join, acknowledge, mark read, renew a lease or commit a mutation.
/// Implementations cap candidate reads at 201 (200 results plus lookahead).
#[async_trait]
pub trait Snapshot: Send {
    /// Committed upper bound captured in the same read transaction.
    fn head(&self) -> Cursor;
    /// Captured shared-order upper bound for mixed Dispatch inbox pagination.
    /// Unlike the audit head, this includes a Notice created after its event.
    fn dispatch_head(&self) -> Cursor;
    /// Retained sealed identity at one sequence, for restore-aware boundaries.
    async fn event_identity(&mut self, seq: Cursor) -> Result<Option<Uuid>>;
    /// Descending root-only or direct-reply candidates, bounded by a fixed head.
    /// Related immutable labels and batched reply counts share this snapshot.
    async fn message_window(
        &mut self,
        channel: Uuid,
        root: Option<Uuid>,
        before: Option<Cursor>,
        through: Cursor,
        limit: usize,
    ) -> Result<Vec<ObservedMessage>>;
    /// At most 201 complete context-free message projections and reply counts.
    async fn observed_messages(
        &mut self,
        ids: &[Uuid],
        through: Cursor,
    ) -> Result<Vec<ObservedMessage>>;
    /// Bounded channel metadata for already selected retained change events.
    async fn channels_by_ids(&mut self, ids: &[Uuid]) -> Result<Vec<Channel>>;
    /// Retained exact conversation, independent of directory/process metadata.
    async fn conversation(&mut self, host: HostKind, session: &str) -> Result<Option<Agent>>;
    /// One retained identity.
    async fn agent(&mut self, id: Uuid) -> Result<Option<Agent>>;
    /// Common typed actor, without registering a missing participant.
    async fn actor(&mut self, id: Uuid) -> Result<Option<Actor>>;
    /// The anonymous project principal, absent before its first Join.
    async fn user(&mut self) -> Result<Option<Actor>>;
    /// Mixed current channel members ordered by immutable first Join.
    async fn actors(
        &mut self,
        after: Cursor,
        limit: usize,
        channel: Uuid,
    ) -> Result<Vec<Indexed<Actor>>>;
    /// One retained handle, including a left identity.
    async fn handle(&mut self, name: &str) -> Result<Option<Agent>>;
    /// Current/last exact incarnation, without refreshing its heartbeat.
    async fn session(&mut self, agent: Uuid) -> Result<Option<Session>>;
    /// Latest retained launch, including terminal authority, in this snapshot.
    async fn latest_launch(&mut self, agent: Uuid) -> Result<Option<Launch>>;
    /// Full-body cursor/head without granting authority or reconciling a retry.
    async fn message_stream(
        &mut self,
        generation: Uuid,
    ) -> Result<Option<crate::delivery::MessageStream>>;
    /// Return a body only if it is the earliest eligible sealed native input
    /// after the settlement cursor; typed notices cannot be skipped. Retrieval
    /// claims no work, authorizes no output and advances no cursor.
    async fn next_message(
        &mut self,
        agent: Uuid,
        after: Sequence,
    ) -> Result<Option<crate::delivery::MessageItem>>;
    /// Read the next native body or event without claiming or advancing it.
    /// All kinds share one committed sequence; no message-only fallback is safe.
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
    /// Exact retained stream head for receipt recovery; never grants authority
    /// for a new native submission.
    async fn retained_native(
        &mut self,
        agent: Uuid,
        delivery: Uuid,
        seq: Sequence,
    ) -> Result<Option<crate::delivery::NativeItem>>;
    /// One persisted wake attempt, without reconciling or advancing its state.
    async fn wake_batch(&mut self, id: Uuid) -> Result<Option<crate::delivery::WakeBatch>>;
    /// Whether a bounded assigned batch still has sealed pending deliveries.
    async fn wake_batch_has_pending(&mut self, agent: Uuid, batch: Uuid) -> Result<bool>;
    /// Currently scoped pending/inflight delivery IDs assigned to one batch,
    /// in immutable event order, for exact final-dispatch comparison.
    async fn wake_batch_deliveries(&mut self, agent: Uuid, batch: Uuid) -> Result<Vec<Uuid>>;
    /// Bounded exact assigned unfinished rows independent of current membership;
    /// final checks compare their immutable IDs and processing deadlines.
    async fn wake_batch_exact_deliveries(
        &mut self,
        agent: Uuid,
        batch: Uuid,
    ) -> Result<Vec<Delivery>>;
    /// Channel lookup by normalized slug.
    async fn channel(&mut self, name: &str) -> Result<Option<Channel>>;
    /// Channel lookup by stable identity.
    async fn channel_by_id(&mut self, id: Uuid) -> Result<Option<Channel>>;
    /// Whether one current project participant belongs to one channel.
    async fn is_member(&mut self, channel: Uuid, agent: Uuid) -> Result<bool>;
    /// Current channel identities for one participant.
    async fn member_channels(&mut self, agent: Uuid) -> Result<Vec<Uuid>>;
    /// Stored project policy, or none for a legacy unset project.
    async fn channel_policy(&mut self) -> Result<Option<crate::project::ProjectChannelPolicy>>;
    /// Effective settings at this exact read snapshot, with no receipt mutation.
    async fn receipt_settings(&mut self) -> Result<crate::project::ProjectReceiptSettings>;
    /// One immutable message; does not create a receipt.
    async fn message(&mut self, id: Uuid) -> Result<Option<Message>>;
    /// One existing explicit receipt, if any.
    async fn receipt(&mut self, message: Uuid, agent: Uuid) -> Result<Option<Receipt>>;
    /// Retained identities ordered by their immutable first join event.
    /// A channel filter includes only its current joined membership.
    async fn agents(
        &mut self,
        after: Cursor,
        limit: usize,
        channel: Option<Uuid>,
    ) -> Result<Vec<Indexed<Agent>>>;
    /// Channels ordered by creation event after applying one captured archive cut.
    async fn channels(
        &mut self,
        after: Cursor,
        limit: usize,
        condition: ChannelListCondition,
        cutoff: Timestamp,
    ) -> Result<Vec<ChannelActivity>>;
    /// Messages in ascending creation order with validated literal filters.
    async fn messages(
        &mut self,
        filter: &MessageFilter,
        after: Cursor,
        limit: usize,
    ) -> Result<Vec<Message>>;
    /// Actual recipients and history-only readers, once per actor, bounded before
    /// hydrating delivery/read details and ordered by immutable first Join.
    async fn recipient_receipts(
        &mut self,
        message: Uuid,
        after: Cursor,
        limit: usize,
    ) -> Result<Vec<Indexed<RecipientReceipt>>>;
    /// Extensible committed events, optionally restricted to one channel.
    async fn events(
        &mut self,
        after: Cursor,
        limit: usize,
        channels: Option<&[Uuid]>,
    ) -> Result<Vec<Event>>;
    /// Pending/inflight recipient rows in referenced event order.
    async fn inbox(&mut self, agent: Uuid, after: Cursor, limit: usize)
    -> Result<Vec<InboxRecord>>;
    /// Logical active groups by default; all retained states when requested.
    async fn reservations(
        &mut self,
        owners: Option<Vec<Uuid>>,
        all: bool,
        now: Timestamp,
        after: Cursor,
        limit: usize,
    ) -> Result<Vec<Reservation>>;
    /// Visible messages without that agent's receipt, excluding its own messages.
    async fn unread_count(&mut self, agent: Uuid) -> Result<u64>;
    /// Bounded scalar status information; does not start dispatch or repair.
    async fn statistics(&mut self) -> Result<ProjectStatistics>;
}
