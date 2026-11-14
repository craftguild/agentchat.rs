//! Complete raw Markdown, safe HTML and independent pinned/window projections.

use super::{Fields, Types};
use crate::projection::{Message, Pinned, Window};
use agentchat_core::{
    Result,
    read::{Checkpoint, IdentityLabel},
};
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, Id};
use std::sync::Arc;

impl contract::MessageResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Arc<Message>) -> Result<Id> {
        Ok(Id::new(p.raw.message.message.id.to_string()))
    }
    async fn channel_id<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<Id> {
        Ok(Id::new(p.raw.message.message.channel_id.to_string()))
    }
    async fn channel<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<String> {
        Ok(p.raw.message.channel.clone())
    }
    async fn author<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<IdentityLabel> {
        Ok(p.raw.message.author.clone())
    }
    async fn root_id<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<Option<Id>> {
        Ok(p.raw
            .message
            .message
            .root_message_id
            .map(|id| Id::new(id.to_string())))
    }
    async fn thread_id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Arc<Message>) -> Result<Id> {
        Ok(Id::new(p.raw.message.thread_id.to_string()))
    }
    async fn creation_seq<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<String> {
        Ok(p.raw.message.message.creation_seq.get().to_string())
    }
    async fn created_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<String> {
        Ok(p.raw.message.message.created_at.to_string())
    }
    async fn body<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Arc<Message>) -> Result<String> {
        Ok(p.raw.message.message.body.clone())
    }
    async fn body_html<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<String> {
        Ok(p.body.html.clone())
    }
    async fn render_fallback<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<bool> {
        Ok(p.body.fallback)
    }
    async fn reply_count<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Message>,
    ) -> Result<String> {
        Ok(p.raw.reply_count.to_string())
    }
}
impl contract::MessageWindowResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn snapshot<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Window>,
    ) -> Result<Checkpoint> {
        Ok(p.snapshot)
    }
    async fn items<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Window>,
    ) -> Result<Vec<Arc<Message>>> {
        Ok(p.items.clone())
    }
    async fn next_before<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Window>,
    ) -> Result<Option<String>> {
        Ok(p.next_before.map(|seq| seq.get().to_string()))
    }
}
impl contract::PinnedRootResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn snapshot<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Pinned>,
    ) -> Result<Checkpoint> {
        Ok(p.snapshot)
    }
    async fn root<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Pinned>,
    ) -> Result<Arc<Message>> {
        Ok(p.root.clone())
    }
}
