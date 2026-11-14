//! Mixed actor membership remains distinct from the native-only agent directory.

use super::{Fields, Types};
use agentchat_core::{
    Result,
    model::{ActorIdentity, ActorKind},
    read::{ActorView, Page},
};
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, Id};

pub(super) fn kind(value: ActorKind) -> contract::ActorKind {
    match value {
        ActorKind::Agent => contract::ActorKind::Agent,
        ActorKind::User => contract::ActorKind::User,
    }
}

impl contract::ActorResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ActorView) -> Result<Id> {
        Ok(Id::new(p.actor.actor_id.to_string()))
    }
    async fn kind<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ActorView,
    ) -> Result<contract::ActorKind> {
        Ok(kind(p.actor.kind()))
    }
    async fn handle<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ActorView) -> Result<String> {
        Ok(p.actor.handle.clone())
    }
    async fn joined<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ActorView) -> Result<bool> {
        Ok(p.actor.joined)
    }
    async fn host<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ActorView,
    ) -> Result<Option<String>> {
        Ok(match &p.actor.identity {
            ActorIdentity::Agent { host, .. } => Some(host.as_str().into()),
            ActorIdentity::User => None,
        })
    }
    async fn chat_id<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ActorView,
    ) -> Result<Option<String>> {
        Ok(match &p.actor.identity {
            ActorIdentity::Agent { chat_id, .. } => Some(chat_id.clone()),
            ActorIdentity::User => None,
        })
    }
}

impl contract::ActorPageResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn items<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<ActorView>,
    ) -> Result<Vec<ActorView>> {
        Ok(p.items.clone())
    }
    async fn next_cursor<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<ActorView>,
    ) -> Result<Option<String>> {
        Ok(p.next_cursor.map(|c| c.get().to_string()))
    }
    async fn scan_cursor<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<ActorView>,
    ) -> Result<String> {
        Ok(p.scan_cursor.get().to_string())
    }
    async fn head<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<ActorView>,
    ) -> Result<String> {
        Ok(p.head.get().to_string())
    }
}
