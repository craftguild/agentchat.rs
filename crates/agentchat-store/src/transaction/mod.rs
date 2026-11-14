//! SeaORM implementation of the application-owned immediate transaction boundary.

pub(crate) mod actors;
pub(crate) mod confirmations;
mod deliveries;
mod departures;
pub(crate) mod dispatches;
mod events;
mod guard_proof;
mod identity;
mod launches;
mod message_stream_state;
pub(crate) mod message_streams;
mod messages;
mod reads;
mod reservations;
pub(crate) mod sequence;
pub(crate) mod wake;

use agentchat_core::delivery::{WakeBatch, WakeItem};
use agentchat_core::{
    Result,
    model::*,
    ports::{IdempotencyRecord, Storage, Transaction},
    sequence::Sequence,
    time::Timestamp,
};
use async_trait::async_trait;
use sea_orm::{
    ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, SqliteTransactionMode,
    TransactionOptions, TransactionTrait,
};
use uuid::Uuid;

use crate::{
    Store, decode,
    entities::event,
    error::{database, integrity},
};

pub(crate) struct StoreTransaction {
    db: DatabaseTransaction,
    guard: guard_proof::Proof,
    root: bool,
}

impl StoreTransaction {
    fn nested(&self, db: DatabaseTransaction) -> Self {
        Self {
            db,
            guard: self.guard.clone(),
            root: false,
        }
    }
}

#[async_trait]
impl Storage for Store {
    fn project_id(&self) -> Uuid {
        self.identity().id
    }

    async fn snapshot(&self) -> Result<Box<dyn agentchat_core::read::Snapshot>> {
        crate::read::begin(&self.reader).await
    }

    async fn wake_candidates(&self, contexts: &[Context], now: Timestamp) -> Result<Vec<Uuid>> {
        crate::wake_candidates::select(&self.reader, contexts, now).await
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        let db = self
            .db
            .begin_with_options(TransactionOptions {
                sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
                ..Default::default()
            })
            .await
            .map_err(database)?;
        Ok(Box::new(StoreTransaction {
            db,
            guard: guard_proof::Proof::default(),
            root: true,
        }))
    }
}

#[async_trait]
impl Transaction for StoreTransaction {
    async fn savepoint(&mut self) -> Result<Box<dyn Transaction + '_>> {
        let db = self.db.begin().await.map_err(database)?;
        Ok(Box::new(self.nested(db)))
    }
    async fn rollback(self: Box<Self>) -> Result<()> {
        self.db.rollback().await.map_err(database)
    }
    async fn idempotency(&mut self, id: Uuid) -> Result<Option<IdempotencyRecord>> {
        events::idempotency(self, id).await
    }
    async fn save_idempotency(&mut self, v: &IdempotencyRecord) -> Result<()> {
        events::save_idempotency(self, v).await
    }
    async fn agent(&mut self, id: Uuid) -> Result<Option<Agent>> {
        identity::agent(self, id).await
    }
    async fn actor(&mut self, id: Uuid) -> Result<Option<Actor>> {
        actors::get(&self.db, id).await
    }
    async fn user(&mut self) -> Result<Option<Actor>> {
        actors::user(&self.db).await
    }
    async fn save_actor(&mut self, actor: &Actor) -> Result<()> {
        actors::save(&self.db, actor).await
    }
    async fn joined_actors(&mut self) -> Result<Vec<Actor>> {
        actors::joined(&self.db, None).await
    }
    async fn receipt_settings(
        &mut self,
    ) -> Result<agentchat_core::project::ProjectReceiptSettings> {
        crate::receipt_settings::get(&self.db).await
    }
    async fn save_receipt_settings(
        &mut self,
        value: &agentchat_core::project::ProjectReceiptSettings,
    ) -> Result<()> {
        crate::receipt_settings::save(&self.db, value).await
    }
    async fn conversation(&mut self, h: HostKind, id: &str) -> Result<Option<Agent>> {
        identity::conversation(self, h, id).await
    }
    async fn handle(&mut self, h: &str) -> Result<Option<Agent>> {
        identity::handle(self, h).await
    }
    async fn joined_agents(&mut self) -> Result<Vec<Agent>> {
        identity::joined(self).await
    }
    async fn save_agent(&mut self, v: &Agent) -> Result<()> {
        identity::save_agent(self, v).await
    }
    async fn latest_launch(&mut self, agent: Uuid) -> Result<Option<Launch>> {
        launches::latest(self, agent).await
    }
    async fn launch(&mut self, generation: Uuid) -> Result<Option<Launch>> {
        launches::get(self, generation).await
    }
    async fn active_launches(&mut self, after: Option<Uuid>) -> Result<Vec<Launch>> {
        launches::active(self, after).await
    }
    async fn save_launch(&mut self, value: &Launch) -> Result<()> {
        launches::save(self, value).await
    }
    async fn session(&mut self, id: Uuid) -> Result<Option<Session>> {
        identity::session(self, id).await
    }
    async fn save_session(&mut self, v: &Session) -> Result<()> {
        identity::save_session(self, v).await
    }
    async fn disconnect_sessions(&mut self) -> Result<()> {
        identity::disconnect_sessions(self).await
    }
    async fn channel(&mut self, n: &str) -> Result<Option<Channel>> {
        identity::channel(self, n).await
    }
    async fn channels(&mut self) -> Result<Vec<Channel>> {
        identity::channels(self).await
    }
    async fn channel_policy(
        &mut self,
    ) -> Result<Option<agentchat_core::project::ProjectChannelPolicy>> {
        identity::channel_policy(self).await
    }
    async fn save_channel_policy(
        &mut self,
        policy: &agentchat_core::project::ProjectChannelPolicy,
    ) -> Result<()> {
        identity::save_channel_policy(self, policy).await
    }
    async fn insert_channel(&mut self, v: &Channel) -> Result<()> {
        identity::insert_channel(self, v).await
    }
    async fn add_member(&mut self, c: Uuid, a: Uuid, s: Sequence) -> Result<()> {
        identity::add_member(self, c, a, s).await
    }
    async fn is_member(&mut self, c: Uuid, a: Uuid) -> Result<bool> {
        identity::is_member(self, c, a).await
    }
    async fn membership_boundary(&mut self, c: Uuid, a: Uuid) -> Result<Option<Sequence>> {
        identity::membership_boundary(self, c, a).await
    }
    async fn remove_member(&mut self, c: Uuid, a: Uuid) -> Result<()> {
        identity::remove_member(self, c, a).await
    }
    async fn cancel_channel_deliveries(&mut self, c: Uuid, a: Uuid, at: Timestamp) -> Result<()> {
        departures::cancel(self, a, Some(c), at).await
    }
    async fn channel_member_count(&mut self, c: Uuid) -> Result<u64> {
        identity::channel_member_count(self, c).await
    }
    async fn channel_members(&mut self, c: Uuid) -> Result<Vec<Agent>> {
        identity::channel_members(self, c).await
    }
    async fn remove_memberships(&mut self, a: Uuid) -> Result<()> {
        identity::remove_memberships(self, a).await
    }
    async fn message(&mut self, id: Uuid) -> Result<Option<Message>> {
        messages::message(self, id).await
    }
    async fn messages(&mut self, ids: &[Uuid]) -> Result<Vec<Message>> {
        reads::messages(self, ids).await
    }
    async fn allocate_dispatch(
        &mut self,
        kind: DispatchKind,
        channel: Option<Uuid>,
        now: Timestamp,
    ) -> Result<Dispatch> {
        dispatches::allocate(self, kind, channel, now).await
    }
    async fn publish_dispatch(
        &mut self,
        header: &Dispatch,
        payload: &DispatchPayload,
        recipients: &[Delivery],
    ) -> Result<()> {
        dispatches::publish(self, header, payload, recipients).await
    }
    async fn dispatch(&mut self, id: Uuid) -> Result<Option<Dispatch>> {
        crate::dispatches::get(&self.db, id).await
    }
    async fn insert_mention(&mut self, v: &Mention) -> Result<()> {
        messages::insert_mention(self, v).await
    }
    async fn receipt(&mut self, m: Uuid, a: Uuid) -> Result<Option<Receipt>> {
        messages::receipt(self, m, a).await
    }
    async fn receipt_kind(&mut self, m: Uuid, a: Uuid, k: ReadKind) -> Result<Option<Receipt>> {
        messages::receipt_kind(self, m, a, k).await
    }
    async fn receipts(&mut self, agent: Uuid, ids: &[Uuid]) -> Result<Vec<Receipt>> {
        reads::receipts(self, agent, ids).await
    }
    async fn insert_receipt(&mut self, v: &Receipt) -> Result<()> {
        messages::insert_receipt(self, v).await
    }
    async fn insert_receipts(&mut self, receipts: &[Receipt]) -> Result<()> {
        reads::insert(self, receipts).await
    }
    async fn due_deliveries(&mut self, now: Timestamp) -> Result<Vec<Delivery>> {
        confirmations::due(self, now).await
    }
    async fn delivery_timeout_notice(&mut self, message: Uuid) -> Result<Option<Uuid>> {
        confirmations::notice(self, message).await
    }
    async fn save_delivery_timeout_notice(&mut self, message: Uuid, event: Uuid) -> Result<()> {
        confirmations::save_notice(self, message, event).await
    }
    async fn expired_recipients(&mut self, message: Uuid) -> Result<ExpiredRecipients> {
        confirmations::expired(self, message).await
    }
    async fn cancel_author_timeout_notices(&mut self, actor: Uuid) -> Result<()> {
        confirmations::cancel_author(self, actor).await
    }
    async fn author_timeout_notice_allowed(
        &mut self,
        actor: Uuid,
        created: Sequence,
    ) -> Result<bool> {
        confirmations::author_allowed(self, actor, created).await
    }
    async fn event(&mut self, id: Uuid) -> Result<Option<Event>> {
        events::get(self, id).await
    }
    async fn delivery(&mut self, id: Uuid) -> Result<Option<Delivery>> {
        crate::delivery_history::get(&self.db, id).await
    }
    async fn deliveries(&mut self, ids: &[Uuid]) -> Result<Vec<Delivery>> {
        deliveries::get(self, ids).await
    }
    async fn save_delivery(&mut self, v: &Delivery) -> Result<()> {
        crate::delivery_history::save(&self.db, v).await
    }
    async fn cancel_deliveries(&mut self, a: Uuid, at: Timestamp) -> Result<()> {
        departures::cancel(self, a, None, at).await
    }
    async fn message_stream(
        &mut self,
        generation: Uuid,
    ) -> Result<Option<agentchat_core::delivery::MessageStream>> {
        message_streams::get(&self.db, generation).await
    }
    async fn save_message_stream(
        &mut self,
        stream: &agentchat_core::delivery::MessageStream,
    ) -> Result<()> {
        message_streams::save(self, stream).await
    }
    async fn next_message(
        &mut self,
        agent: Uuid,
        after: Sequence,
    ) -> Result<Option<agentchat_core::delivery::MessageItem>> {
        message_streams::next(&self.db, agent, after).await
    }
    async fn next_native(
        &mut self,
        agent: Uuid,
        after: Sequence,
    ) -> Result<Option<agentchat_core::delivery::NativeItem>> {
        message_streams::next_native(&self.db, agent, after).await
    }
    async fn retained_native(
        &mut self,
        agent: Uuid,
        delivery: Uuid,
        seq: Sequence,
    ) -> Result<Option<agentchat_core::delivery::NativeItem>> {
        message_streams::retained(&self.db, agent, delivery, seq).await
    }
    async fn wake_batch(&mut self, id: Uuid) -> Result<Option<WakeBatch>> {
        wake::get(&self.db, id).await
    }
    async fn outstanding_wake(
        &mut self,
        agent: Uuid,
        incarnation: Uuid,
    ) -> Result<Option<WakeBatch>> {
        wake::outstanding(self, agent, incarnation).await
    }
    async fn save_wake_batch(&mut self, batch: &WakeBatch) -> Result<()> {
        wake::save(self, batch).await
    }
    async fn wake_batch_has_pending(&mut self, agent: Uuid, batch: Uuid) -> Result<bool> {
        wake::has_pending_exact(self, agent, batch).await
    }
    async fn wake_items(&mut self, agent: Uuid, batch: Option<Uuid>) -> Result<Vec<WakeItem>> {
        wake::items(self, agent, batch).await
    }
    async fn pending_delivery_count(&mut self, agent: Uuid) -> Result<u64> {
        wake::count(self, agent).await
    }
    async fn retire_wake_incarnations(&mut self, agent: Uuid, current: Uuid) -> Result<()> {
        wake::retire(self, agent, current).await
    }
    async fn allocate_event(&mut self, k: EventKind, t: Timestamp) -> Result<Event> {
        events::allocate(self, k, t).await
    }
    async fn publish_event(&mut self, e: &Event) -> Result<()> {
        events::publish(self, e).await
    }
    async fn reservation(&mut self, id: Uuid) -> Result<Option<Reservation>> {
        reservations::get(self, id).await
    }
    async fn active_reservations(&mut self, after: Option<Uuid>) -> Result<Vec<Reservation>> {
        reservations::active(self, after).await
    }
    async fn save_reservation(&mut self, v: &Reservation) -> Result<()> {
        reservations::save(self, v).await
    }
    async fn commit(self: Box<Self>) -> Result<()> {
        self.guard.check(&self.db, self.root).await?;
        if event::Entity::find()
            .filter(event::Column::Sealed.eq(false))
            .one(&self.db)
            .await
            .map_err(database)?
            .is_some()
        {
            return Err(integrity("Cannot commit an unpublished business event."));
        }
        self.db.commit().await.map_err(database)
    }
}
