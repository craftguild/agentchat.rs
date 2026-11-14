//! Explicit test-owned launch bindings, independent of any real host admission.
use super::SyntheticHost;
use agentchat_core::{
    Result,
    delivery::InputCapabilities,
    model::{Context, LaunchIdentity, LaunchOwnerState},
    ports::{HostGate, HostState},
};
use std::{collections::HashMap, sync::Mutex};
use uuid::Uuid;

#[derive(Default)]
pub struct NativeHost(Mutex<HashMap<Uuid, LaunchIdentity>>);
impl NativeHost {
    pub fn bind(&self, context: &Context) {
        self.0.lock().unwrap().insert(
            context.context_id,
            LaunchIdentity {
                generation: Uuid::new_v4(),
                owner: format!("explicit-test-owner-{}", context.context_id),
            },
        );
    }
}
#[async_trait::async_trait]
impl HostGate for NativeHost {
    fn input_capabilities(&self, context: &Context) -> Result<InputCapabilities> {
        SyntheticHost.input_capabilities(context)
    }
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self.0.lock().unwrap().get(&context.context_id).cloned())
    }
    fn launch_owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
        if self
            .0
            .lock()
            .unwrap()
            .values()
            .any(|current| current == identity)
        {
            LaunchOwnerState::Current
        } else {
            LaunchOwnerState::Unknown
        }
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        SyntheticHost.verify(context).await
    }
}
