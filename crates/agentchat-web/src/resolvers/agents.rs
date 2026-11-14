//! Minimal participant projection; only the host kind is exposed to presentation.
use super::{Fields, Types};
use agentchat_core::{
    Result,
    read::{AgentView, Page},
};
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, Id};

impl contract::AgentResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn kind<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        _: &'a AgentView,
    ) -> Result<contract::ActorKind> {
        Ok(contract::ActorKind::Agent)
    }
    async fn id<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a AgentView) -> Result<Id> {
        Ok(Id::new(p.agent.id.to_string()))
    }
    async fn handle<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a AgentView) -> Result<String> {
        Ok(p.agent.handle.clone())
    }
    async fn host<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a AgentView) -> Result<String> {
        Ok(p.agent.host.as_str().to_owned())
    }
    async fn joined<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a AgentView) -> Result<bool> {
        Ok(p.agent.joined)
    }
}
impl contract::AgentPageResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn items<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<AgentView>,
    ) -> Result<Vec<AgentView>> {
        Ok(p.items.clone())
    }
    async fn next_cursor<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<AgentView>,
    ) -> Result<Option<String>> {
        Ok(p.next_cursor.map(|seq| seq.get().to_string()))
    }
    async fn scan_cursor<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<AgentView>,
    ) -> Result<String> {
        Ok(p.scan_cursor.get().to_string())
    }
    async fn head<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<AgentView>,
    ) -> Result<String> {
        Ok(p.head.get().to_string())
    }
}
