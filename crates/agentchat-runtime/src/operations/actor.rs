//! Public actor selection resolves current authority without creating participation.

use super::*;
use agentchat_core::{
    model::{ActorSelector, HostKind, LaunchOwnerState},
    ports::HostGate,
};
use agentchat_protocol::rpc::ActorReference;

impl Route {
    pub(super) fn actor_reference(&self) -> Option<ActorReference> {
        if self.user {
            Some(ActorReference::User {})
        } else {
            self.context.as_ref().map(|context| ActorReference::Native {
                context_id: context.context_id,
            })
        }
    }
}

impl Operations {
    pub(super) async fn select_native(
        &self,
        project: Uuid,
        selector: &ActorSelector,
    ) -> Result<Context> {
        let ActorSelector::Native { host, chat_id } = selector else {
            unreachable!("native selector")
        };
        selector.validate()?;
        let registry = ContextRegistry::new(self.manager.clone());
        let candidates = registry.matching(project, *host, chat_id).await?;
        let gate = agentchat_hosts::AdmittedHosts::new(self.manager.home().to_owned());
        let mut selected = None;
        let mut failure = None;
        for context in candidates {
            let proof = async {
                registry.resolve(context.context_id, Some(project)).await?;
                let identity = gate.launch_identity(&context).await?.ok_or_else(required)?;
                if gate.launch_owner(&identity) != LaunchOwnerState::Current
                    || gate.launch_root_departed(project, *host, chat_id, &identity)
                    || gate.launch_root_changed(project, *host, chat_id, &identity)
                {
                    return Err(required());
                }
                // Selection checks the current published route without native
                // observation/phase renewal. The application reauthorizes every
                // mutation; whoami itself remains a read-only identity lookup.
                match host {
                    HostKind::Codex => {
                        agentchat_hosts::codex::NativeRegistry::new(self.manager.home())
                            .observe(&context)?;
                    }
                    HostKind::Claude => {
                        agentchat_hosts::claude::adapter::NativeAdapter::new(
                            self.manager.home().into(),
                        )
                        .check_context(&context)
                        .await?;
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = proof {
                failure = Some(error);
                continue;
            }
            if selected.replace(context).is_some() {
                return Err(Error::new(
                    ErrorCode::IdentityConflict,
                    "The selected native actor has ambiguous current host associations.",
                ));
            }
        }
        selected.ok_or_else(|| failure.unwrap_or_else(required))
    }
}

fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The selected project/host/chat actor has no exact current managed association.",
    )
}
