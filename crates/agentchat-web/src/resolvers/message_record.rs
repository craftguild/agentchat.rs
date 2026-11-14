//! Raw canonical detail fields do not invent rendered-window reply aggregates.

use super::{Fields, Types};
use agentchat_core::{
    Result,
    read::{IdentityLabel, MessageView},
};
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, Id};

impl contract::MessageRecordResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a MessageView) -> Result<Id> {
        Ok(Id::new(p.message.id.to_string()))
    }
    async fn channel_id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a MessageView) -> Result<Id> {
        Ok(Id::new(p.message.channel_id.to_string()))
    }
    async fn channel<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MessageView,
    ) -> Result<String> {
        Ok(p.channel.clone())
    }
    async fn author<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MessageView,
    ) -> Result<IdentityLabel> {
        Ok(p.author.clone())
    }
    async fn root_id<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MessageView,
    ) -> Result<Option<Id>> {
        Ok(p.message.root_message_id.map(|id| Id::new(id.to_string())))
    }
    async fn thread_id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a MessageView) -> Result<Id> {
        Ok(Id::new(p.thread_id.to_string()))
    }
    async fn creation_seq<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MessageView,
    ) -> Result<String> {
        Ok(p.message.creation_seq.get().to_string())
    }
    async fn created_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MessageView,
    ) -> Result<String> {
        Ok(p.message.created_at.to_string())
    }
    async fn body<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a MessageView) -> Result<String> {
        Ok(p.message.body.clone())
    }
}
