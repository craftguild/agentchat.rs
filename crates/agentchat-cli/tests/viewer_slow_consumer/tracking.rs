//! Instrument only the injected typed source; storage/runtime policy is unchanged.
use agentchat_core::{
    Result,
    read::{
        ActorView, AgentView, ChangeBatch, ChangeRequest, ChannelListCondition, ChannelPage,
        Checkpoint, MessageDetail, MessageWindow, Page, PageRequest, PinnedRoot, ViewerChangeKind,
        requests,
    },
};
use agentchat_protocol::viewer::{ChangeObserver, ViewerReader};
use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use uuid::Uuid;

#[derive(Default)]
pub struct Progress {
    pub messages: AtomicUsize,
    pub alive: AtomicBool,
}
pub struct Tracked {
    pub reader: Arc<dyn ViewerReader>,
    pub progress: Arc<Progress>,
}
struct Source {
    source: Box<dyn ChangeObserver>,
    progress: Arc<Progress>,
}
impl Drop for Source {
    fn drop(&mut self) {
        self.progress.alive.store(false, Ordering::SeqCst);
    }
}
#[async_trait]
impl ChangeObserver for Source {
    fn checkpoint(&self) -> Checkpoint {
        self.source.checkpoint()
    }
    async fn next(&mut self) -> Result<ChangeBatch> {
        let batch = self.source.next().await?;
        let messages = batch
            .items
            .iter()
            .filter(|c| matches!(&c.change, ViewerChangeKind::MessageCreated { .. }))
            .count();
        self.progress.messages.fetch_add(messages, Ordering::SeqCst);
        Ok(batch)
    }
}
#[async_trait]
impl ViewerReader for Tracked {
    fn project_id(&self) -> Uuid {
        self.reader.project_id()
    }
    async fn head(&self) -> Result<Checkpoint> {
        self.reader.head().await
    }
    async fn channels(
        &self,
        condition: ChannelListCondition,
        page: PageRequest,
    ) -> Result<ChannelPage> {
        self.reader.channels(condition, page).await
    }
    async fn agents(&self, p: PageRequest) -> Result<Page<AgentView>> {
        self.reader.agents(p).await
    }
    async fn channel_members(&self, p: requests::ChannelMembers) -> Result<Page<ActorView>> {
        self.reader.channel_members(p).await
    }
    async fn message(&self, p: requests::Message) -> Result<MessageDetail> {
        self.reader.message(p).await
    }
    async fn roots(&self, p: requests::RootWindow) -> Result<MessageWindow> {
        self.reader.roots(p).await
    }
    async fn pinned(&self, p: requests::PinnedRoot) -> Result<PinnedRoot> {
        self.reader.pinned(p).await
    }
    async fn replies(&self, p: requests::ReplyWindow) -> Result<MessageWindow> {
        self.reader.replies(p).await
    }
    async fn changes(&self, p: ChangeRequest) -> Result<ChangeBatch> {
        self.reader.changes(p).await
    }
    async fn watch(&self, p: Checkpoint) -> Result<Box<dyn ChangeObserver>> {
        let source = self.reader.watch(p).await?;
        self.progress.alive.store(true, Ordering::SeqCst);
        Ok(Box::new(Source {
            source,
            progress: self.progress.clone(),
        }))
    }
}
