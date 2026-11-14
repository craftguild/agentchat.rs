//! Bounded exact project/host/chat selection over retained registry descriptors.

use super::*;

impl ContextRegistry {
    /// Find immutable matching records without registering an actor or choosing a
    /// current native owner. The operation adapter must independently prove the
    /// current published association; retained records alone grant no authority.
    pub async fn matching(
        &self,
        project: Uuid,
        host: HostKind,
        chat_id: &str,
    ) -> Result<Vec<Context>> {
        agentchat_core::model::ActorSelector::Native {
            host,
            chat_id: chat_id.to_owned(),
        }
        .validate()?;
        let manager = self.manager.clone();
        let ids = blocking(move || Self::removal_records(&manager, project)).await?;
        let mut matching = Vec::new();
        for id in ids {
            let context = self.descriptor(id).await?;
            if context.project_id == project
                && context.host == host
                && context.host_session_id == chat_id
            {
                matching.push(context);
            }
        }
        Ok(matching)
    }
}
