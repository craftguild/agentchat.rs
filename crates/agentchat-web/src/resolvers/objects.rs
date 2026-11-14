//! Scalar and compact collection projections preserve shared DTO types.

use super::{Fields, Types};
use agentchat_core::{
    Result,
    model::Channel,
    read::{ChannelPage, ChannelView, Checkpoint, IdentityLabel},
};
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, Id};

impl contract::CheckpointResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn project_id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Checkpoint) -> Result<Id> {
        Ok(Id::new(p.project_id.to_string()))
    }
    async fn seq<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Checkpoint) -> Result<String> {
        Ok(p.seq.get().to_string())
    }
    async fn event_id<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Checkpoint,
    ) -> Result<Option<Id>> {
        Ok(p.event_id.map(|id| Id::new(id.to_string())))
    }
}
impl contract::ProjectResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Checkpoint) -> Result<Id> {
        Ok(Id::new(p.project_id.to_string()))
    }
    async fn checkpoint<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Checkpoint,
    ) -> Result<Checkpoint> {
        Ok(*p)
    }
}
impl contract::ChannelResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Channel) -> Result<Id> {
        Ok(Id::new(p.id.to_string()))
    }
    async fn name<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Channel) -> Result<String> {
        Ok(p.name.clone())
    }
    async fn creation_seq<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Channel,
    ) -> Result<String> {
        Ok(p.creation_seq.get().to_string())
    }
}
impl contract::ChannelViewResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ChannelView) -> Result<Id> {
        Ok(Id::new(p.channel.id.to_string()))
    }
    async fn name<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ChannelView) -> Result<String> {
        Ok(p.channel.name.clone())
    }
    async fn creation_seq<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ChannelView,
    ) -> Result<String> {
        Ok(p.channel.creation_seq.get().to_string())
    }
    async fn last_conversation_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ChannelView,
    ) -> Result<String> {
        Ok(p.last_conversation_at.to_string())
    }
    async fn archive_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ChannelView,
    ) -> Result<String> {
        Ok(p.archive_at.to_string())
    }
    async fn archived<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ChannelView) -> Result<bool> {
        Ok(p.archived)
    }
    async fn mandatory<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ChannelView,
    ) -> Result<bool> {
        Ok(p.mandatory)
    }
}
impl contract::ChannelPageResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn items<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ChannelPage,
    ) -> Result<Vec<ChannelView>> {
        Ok(p.page.items.clone())
    }
    async fn next_cursor<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ChannelPage,
    ) -> Result<Option<String>> {
        Ok(p.page.next_cursor.map(|seq| seq.get().to_string()))
    }
    async fn scan_cursor<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ChannelPage,
    ) -> Result<String> {
        Ok(p.page.scan_cursor.get().to_string())
    }
    async fn head<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ChannelPage) -> Result<String> {
        Ok(p.page.head.get().to_string())
    }
    async fn as_of<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ChannelPage) -> Result<String> {
        Ok(p.as_of.to_string())
    }
}
impl contract::AuthorResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a IdentityLabel) -> Result<Id> {
        Ok(Id::new(p.actor_id.to_string()))
    }
    async fn kind<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a IdentityLabel,
    ) -> Result<contract::ActorKind> {
        Ok(super::actors::kind(p.kind))
    }
    async fn handle<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a IdentityLabel,
    ) -> Result<String> {
        Ok(p.handle.clone())
    }
}
