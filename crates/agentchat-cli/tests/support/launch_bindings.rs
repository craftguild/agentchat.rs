//! Immutable launch identities selected explicitly by offline fixtures.
use agentchat_core::model::{Context, LaunchIdentity, LaunchOwnerState};
use std::{collections::HashMap, sync::Mutex};
use uuid::Uuid;

#[derive(Default)]
pub struct LaunchBindings(Mutex<HashMap<Uuid, LaunchIdentity>>);
impl LaunchBindings {
    pub fn bind(&self, context: &Context) {
        self.0
            .lock()
            .unwrap()
            .entry(context.context_id)
            .or_insert_with(|| LaunchIdentity {
                generation: Uuid::new_v4(),
                owner: format!("explicit-offline-fixture-{}", context.context_id),
            });
    }
    pub fn identity(&self, context: &Context) -> Option<LaunchIdentity> {
        self.0.lock().unwrap().get(&context.context_id).cloned()
    }
    pub fn owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
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
}
