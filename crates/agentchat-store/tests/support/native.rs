//! Test-owned exact generations; no real host identity or profile is admitted.
use super::{Fixture, SyntheticHost};
use agentchat_core::{
    Result,
    application::Application,
    delivery::{ActiveInput, InputCapabilities, InputRecovery},
    model::{Context, LaunchIdentity, LaunchOwnerState},
    ports::{HostGate, HostState},
};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub struct Gate {
    host: Arc<SyntheticHost>,
    identities: Mutex<HashMap<Uuid, LaunchIdentity>>,
}
impl Gate {
    pub fn new(f: &Fixture) -> Arc<Self> {
        Arc::new(Self {
            host: f.host.clone(),
            identities: Mutex::new(HashMap::new()),
        })
    }
    pub fn bind(&self, context: &Context) -> LaunchIdentity {
        let identity = LaunchIdentity {
            generation: Uuid::new_v4(),
            owner: "receipt-policy-test-owner".into(),
        };
        self.identities
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
        identity
    }
    pub fn reconnect(&self, context: &Context, identity: &LaunchIdentity) {
        self.identities
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
    }
    pub fn app(self: &Arc<Self>, f: &Fixture) -> Application {
        Application::new(f.store.clone(), f.clock.clone(), self.clone())
    }
}
#[async_trait]
impl HostGate for Gate {
    fn input_capabilities(&self, _: &Context) -> Result<InputCapabilities> {
        Ok(InputCapabilities::complete(
            InputRecovery::OriginalReceipt,
            ActiveInput::ExactTurnId,
        ))
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        self.host.verify(context).await
    }
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self
            .identities
            .lock()
            .unwrap()
            .get(&context.context_id)
            .cloned())
    }
    fn launch_owner(&self, _: &LaunchIdentity) -> LaunchOwnerState {
        LaunchOwnerState::Current
    }
}
