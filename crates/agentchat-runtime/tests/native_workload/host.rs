//! Explicit test-only launch authority; no process or production profile claim.
use agentchat_core::{
    Result,
    model::{Context, LaunchIdentity, LaunchOwnerState},
    ports::{HostGate, HostState},
};
use std::collections::HashMap;
use uuid::Uuid;
pub struct ManagedHost(HashMap<Uuid, LaunchIdentity>);
impl ManagedHost {
    pub fn new(contexts: &[Context]) -> Self {
        Self(
            contexts
                .iter()
                .map(|c| {
                    (
                        c.context_id,
                        LaunchIdentity {
                            generation: Uuid::new_v4(),
                            owner: format!("synthetic-workload-owner-{}", c.context_id),
                        },
                    )
                })
                .collect(),
        )
    }
    pub fn generations(&self) -> HashMap<Uuid, Uuid> {
        self.0.iter().map(|(c, i)| (*c, i.generation)).collect()
    }
}
#[async_trait::async_trait]
impl HostGate for ManagedHost {
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::RetainUncertainty,
            ActiveInput::ExactTurnId,
        ))
    }
    async fn verify(&self, c: &Context) -> Result<HostState> {
        assert!(self.0.contains_key(&c.context_id));
        super::support::SyntheticHost.verify(c).await
    }
    async fn launch_identity(&self, c: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self.0.get(&c.context_id).cloned())
    }
    fn launch_owner(&self, i: &LaunchIdentity) -> LaunchOwnerState {
        if self.0.values().any(|v| v == i) {
            LaunchOwnerState::Current
        } else {
            LaunchOwnerState::Unknown
        }
    }
}
