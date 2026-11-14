//! Explicit human actions, separate from side-effect-free viewer reads.

use agentchat_core::{Result, mentions::MentionWarning};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Successful channel creation (including an existing channel of that name).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedChannel {
    /// Stable channel identity.
    pub channel_id: Uuid,
    /// Canonical channel slug.
    pub name: String,
}

/// A successful immutable post; warnings never mean the post should be repeated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostedMessage {
    /// Exact message to reveal after successful submission.
    pub message_id: Uuid,
    /// Frozen mention diagnostics from the original shared operation.
    pub warnings: Vec<MentionWarning>,
}

/// Fixed-project human actions. Implementations select only the common user,
/// using shared participation and mutation operations rather than native authority.
#[async_trait]
pub trait ViewerWriter: Send + Sync {
    /// Explicitly join, create the channel and join its membership.
    async fn create_channel(&self, request_id: Uuid, name: String) -> Result<CreatedChannel>;
    /// Explicitly join and post in an existing channel.
    async fn post(&self, request_id: Uuid, channel: String, body: String) -> Result<PostedMessage>;
    /// Reply directly to the verified root, joining its existing channel first.
    async fn reply(&self, request_id: Uuid, root: Uuid, body: String) -> Result<PostedMessage>;
}
