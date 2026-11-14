//! Injected context-free reads/observation; Web adapters need no runtime/storage.

use agentchat_core::{
    Result,
    read::{
        ActorView, AgentView, ChangeBatch, ChangeRequest, ChannelListCondition, ChannelPage,
        Checkpoint, MessageDetail, MessageWindow, Page, PageRequest, PinnedRoot, requests,
    },
};
use async_trait::async_trait;
use uuid::Uuid;

/// Owned lossless change source. Dropping it cancels its observation resources.
#[async_trait]
pub trait ChangeObserver: Send {
    /// Last completely received batch boundary, never a browser/application ACK.
    fn checkpoint(&self) -> Checkpoint;
    /// Await the next complete typed batch; cancellation retains partial framing.
    async fn next(&mut self) -> Result<ChangeBatch>;
}

/// Common typed viewer capability supplied by executable composition. Every
/// method is context-free and may not join, ACK/read, renew or wake a native host.
#[async_trait]
pub trait ViewerReader: Send + Sync {
    /// Fixed initialized project for the lifetime of this reader.
    fn project_id(&self) -> Uuid;
    /// Capture a restore-aware snapshot boundary.
    async fn head(&self) -> Result<Checkpoint>;
    /// Existing ascending retained channel pages.
    async fn channels(
        &self,
        condition: ChannelListCondition,
        page: PageRequest,
    ) -> Result<ChannelPage>;
    /// Retained project identities and explicit participation, without joining.
    async fn agents(&self, page: PageRequest) -> Result<Page<AgentView>>;
    /// Current joined channel members through the shared context-free read.
    async fn channel_members(&self, request: requests::ChannelMembers) -> Result<Page<ActorView>>;
    /// Complete immutable message and canonical paged receipts; no receipt writes.
    async fn message(&self, request: requests::Message) -> Result<MessageDetail>;
    /// Latest/backward roots and coherent total reply counts.
    async fn roots(&self, request: requests::RootWindow) -> Result<MessageWindow>;
    /// Pinned root, independently byte bounded from its replies.
    async fn pinned(&self, request: requests::PinnedRoot) -> Result<PinnedRoot>;
    /// Latest/backward direct replies with their own cursor.
    async fn replies(&self, request: requests::ReplyWindow) -> Result<MessageWindow>;
    /// Finite retained replay through the same application policy as observation.
    async fn changes(&self, request: ChangeRequest) -> Result<ChangeBatch>;
    /// Register before durable catch-up, then await coalesced committed hints.
    async fn watch(&self, after: Checkpoint) -> Result<Box<dyn ChangeObserver>>;
}
