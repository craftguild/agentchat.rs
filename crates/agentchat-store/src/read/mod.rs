//! Read-only SeaORM transactions anchored by a committed event head.

mod events;
mod identity;
mod messages;
mod receipts;
mod summaries;
mod viewer;

use agentchat_core::{
    Result,
    model::*,
    read::{
        ChannelActivity, ChannelListCondition, InboxRecord, Indexed, MessageFilter,
        ObservedMessage, ProjectStatistics, Snapshot,
    },
    sequence::{Cursor, Sequence},
    time::Timestamp,
};
use async_trait::async_trait;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, TransactionTrait,
    sea_query::{Alias, Expr, ExprTrait, Func, Query},
};
use uuid::Uuid;

use crate::error::{database, integrity};

pub(super) struct ReadSnapshot {
    db: DatabaseTransaction,
    head: Cursor,
    dispatch_head: Cursor,
}

pub(crate) async fn begin(pool: &DatabaseConnection) -> Result<Box<dyn Snapshot>> {
    let db = pool.begin().await.map_err(database)?;
    // This is the first read in a deferred transaction. All later lookups,
    // aggregates and page candidates see exactly this SQLite WAL snapshot.
    let row = db
        .query_one(
            &Query::select()
                .expr_as(
                    Func::if_null(Func::max(Expr::col(Alias::new("seq"))), 0),
                    Alias::new("head"),
                )
                .from(Alias::new("events"))
                .and_where(Expr::col(Alias::new("sealed")).eq(true))
                .to_owned(),
        )
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Missing event head aggregate."))?;
    let head = Cursor::new(row.try_get("", "head").map_err(database)?)
        .map_err(|_| integrity("Invalid committed event head."))?;
    let dispatch_head = crate::transaction::sequence::head(&db).await?;
    if dispatch_head < head {
        return Err(integrity("Sequence allocator is behind its audit head."));
    }
    Ok(Box::new(ReadSnapshot {
        db,
        head,
        dispatch_head,
    }))
}

pub(super) fn fetch_limit(limit: usize) -> Result<u64> {
    if !(1..=201).contains(&limit) {
        return Err(integrity(
            "Snapshot fetch exceeds its bounded candidate limit.",
        ));
    }
    Ok(limit as u64)
}

#[async_trait]
impl Snapshot for ReadSnapshot {
    fn head(&self) -> Cursor {
        self.head
    }
    fn dispatch_head(&self) -> Cursor {
        self.dispatch_head
    }
    async fn event_identity(&mut self, seq: Cursor) -> Result<Option<Uuid>> {
        viewer::event_identity(self, seq).await
    }
    async fn message_window(
        &mut self,
        channel: Uuid,
        root: Option<Uuid>,
        before: Option<Cursor>,
        through: Cursor,
        limit: usize,
    ) -> Result<Vec<ObservedMessage>> {
        viewer::window(self, channel, root, before, through, limit).await
    }
    async fn observed_messages(
        &mut self,
        ids: &[Uuid],
        through: Cursor,
    ) -> Result<Vec<ObservedMessage>> {
        viewer::by_ids(self, ids, through).await
    }
    async fn channels_by_ids(&mut self, ids: &[Uuid]) -> Result<Vec<Channel>> {
        viewer::channels(self, ids).await
    }
    async fn conversation(&mut self, host: HostKind, id: &str) -> Result<Option<Agent>> {
        identity::conversation(self, host, id).await
    }
    async fn agent(&mut self, id: Uuid) -> Result<Option<Agent>> {
        identity::agent(self, id).await
    }
    async fn actor(&mut self, id: Uuid) -> Result<Option<Actor>> {
        crate::transaction::actors::get(&self.db, id).await
    }
    async fn user(&mut self) -> Result<Option<Actor>> {
        crate::transaction::actors::user(&self.db).await
    }
    async fn actors(
        &mut self,
        after: Cursor,
        limit: usize,
        channel: Uuid,
    ) -> Result<Vec<Indexed<Actor>>> {
        identity::actors(self, after, limit, channel).await
    }
    async fn receipt_settings(
        &mut self,
    ) -> Result<agentchat_core::project::ProjectReceiptSettings> {
        crate::receipt_settings::get(&self.db).await
    }
    async fn handle(&mut self, name: &str) -> Result<Option<Agent>> {
        identity::handle(self, name).await
    }
    async fn session(&mut self, agent: Uuid) -> Result<Option<Session>> {
        identity::session(self, agent).await
    }
    async fn latest_launch(&mut self, agent: Uuid) -> Result<Option<Launch>> {
        identity::latest_launch(self, agent).await
    }
    async fn message_stream(
        &mut self,
        generation: Uuid,
    ) -> Result<Option<agentchat_core::delivery::MessageStream>> {
        crate::transaction::message_streams::get(&self.db, generation).await
    }
    async fn wake_batch(
        &mut self,
        id: Uuid,
    ) -> Result<Option<agentchat_core::delivery::WakeBatch>> {
        crate::transaction::wake::get(&self.db, id).await
    }
    async fn next_message(
        &mut self,
        agent: Uuid,
        after: Sequence,
    ) -> Result<Option<agentchat_core::delivery::MessageItem>> {
        crate::transaction::message_streams::next(&self.db, agent, after).await
    }
    async fn next_native(
        &mut self,
        agent: Uuid,
        after: Sequence,
    ) -> Result<Option<agentchat_core::delivery::NativeItem>> {
        crate::transaction::message_streams::next_native(&self.db, agent, after).await
    }
    async fn retained_native(
        &mut self,
        agent: Uuid,
        delivery: Uuid,
        seq: Sequence,
    ) -> Result<Option<agentchat_core::delivery::NativeItem>> {
        crate::transaction::message_streams::retained(&self.db, agent, delivery, seq).await
    }
    async fn wake_batch_has_pending(&mut self, agent: Uuid, batch: Uuid) -> Result<bool> {
        crate::transaction::wake::has_pending(&self.db, agent, batch).await
    }
    async fn wake_batch_deliveries(&mut self, agent: Uuid, batch: Uuid) -> Result<Vec<Uuid>> {
        crate::transaction::wake::scoped_delivery_ids(&self.db, agent, batch).await
    }
    async fn wake_batch_exact_deliveries(
        &mut self,
        agent: Uuid,
        batch: Uuid,
    ) -> Result<Vec<Delivery>> {
        crate::transaction::wake::selected_deliveries(&self.db, agent, Some(batch), true, false)
            .await
    }
    async fn channel(&mut self, name: &str) -> Result<Option<Channel>> {
        identity::channel(self, name).await
    }
    async fn channel_by_id(&mut self, id: Uuid) -> Result<Option<Channel>> {
        identity::channel_by_id(self, id).await
    }
    async fn is_member(&mut self, channel: Uuid, agent: Uuid) -> Result<bool> {
        identity::is_member(self, channel, agent).await
    }
    async fn member_channels(&mut self, agent: Uuid) -> Result<Vec<Uuid>> {
        identity::member_channels(self, agent).await
    }
    async fn channel_policy(
        &mut self,
    ) -> Result<Option<agentchat_core::project::ProjectChannelPolicy>> {
        identity::channel_policy(self).await
    }
    async fn message(&mut self, id: Uuid) -> Result<Option<Message>> {
        messages::message(self, id).await
    }
    async fn receipt(&mut self, message: Uuid, agent: Uuid) -> Result<Option<Receipt>> {
        messages::receipt(self, message, agent).await
    }
    async fn agents(
        &mut self,
        after: Cursor,
        limit: usize,
        channel: Option<Uuid>,
    ) -> Result<Vec<Indexed<Agent>>> {
        identity::agents(self, after, limit, channel).await
    }
    async fn channels(
        &mut self,
        after: Cursor,
        limit: usize,
        condition: ChannelListCondition,
        cutoff: Timestamp,
    ) -> Result<Vec<ChannelActivity>> {
        identity::channels(self, after, limit, condition, cutoff).await
    }
    async fn messages(
        &mut self,
        filter: &MessageFilter,
        after: Cursor,
        limit: usize,
    ) -> Result<Vec<Message>> {
        messages::list(self, filter, after, limit).await
    }
    async fn recipient_receipts(
        &mut self,
        message: Uuid,
        after: Cursor,
        limit: usize,
    ) -> Result<Vec<Indexed<agentchat_core::read::RecipientReceipt>>> {
        receipts::page(&self.db, self.head, message, after, limit).await
    }
    async fn events(
        &mut self,
        after: Cursor,
        limit: usize,
        channels: Option<&[Uuid]>,
    ) -> Result<Vec<Event>> {
        events::list(self, after, limit, channels).await
    }
    async fn inbox(
        &mut self,
        agent: Uuid,
        after: Cursor,
        limit: usize,
    ) -> Result<Vec<InboxRecord>> {
        events::inbox(self, agent, after, limit).await
    }
    async fn reservations(
        &mut self,
        owners: Option<Vec<Uuid>>,
        all: bool,
        now: Timestamp,
        after: Cursor,
        limit: usize,
    ) -> Result<Vec<Reservation>> {
        summaries::reservations(self, owners, all, now, after, limit).await
    }
    async fn unread_count(&mut self, agent: Uuid) -> Result<u64> {
        summaries::unread(self, agent).await
    }
    async fn statistics(&mut self) -> Result<ProjectStatistics> {
        summaries::statistics(self).await
    }
}
