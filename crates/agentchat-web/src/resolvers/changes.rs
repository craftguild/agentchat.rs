//! Change projections contain complete messages or a newly created channel.

use super::{Fields, Types};
use crate::projection::{Change, Changes, Message};
use agentchat_core::{Result, model::Channel, read::Checkpoint};
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, Id};
use std::sync::Arc;

impl contract::ChangeResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn event_id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Arc<Change>) -> Result<Id> {
        Ok(Id::new(p.event_id.to_string()))
    }
    async fn seq<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a Arc<Change>) -> Result<String> {
        Ok(p.seq.get().to_string())
    }
    async fn kind<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Change>,
    ) -> Result<contract::ChangeKind> {
        Ok(if p.channel.is_some() {
            contract::ChangeKind::ChannelCreated
        } else {
            contract::ChangeKind::MessageCreated
        })
    }
    async fn channel<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Change>,
    ) -> Result<Option<Channel>> {
        Ok(p.channel.clone())
    }
    async fn message<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Change>,
    ) -> Result<Option<Arc<Message>>> {
        Ok(p.message.clone())
    }
}
impl contract::ChangeBatchResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn after<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Changes>,
    ) -> Result<Checkpoint> {
        Ok(p.after)
    }
    async fn scan<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Changes>,
    ) -> Result<Checkpoint> {
        Ok(p.scan)
    }
    async fn head<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Changes>,
    ) -> Result<Checkpoint> {
        Ok(p.head)
    }
    async fn items<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Changes>,
    ) -> Result<Vec<Arc<Change>>> {
        Ok(p.items.clone())
    }
    async fn has_more<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Arc<Changes>,
    ) -> Result<bool> {
        Ok(p.has_more)
    }
}
