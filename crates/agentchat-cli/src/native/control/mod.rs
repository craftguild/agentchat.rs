//! Native callbacks derive their private scope from the exact direct parent.
mod input;
mod lifecycle;
mod transport;
use crate::claude_gateway::records::Control;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::claude::{namespace::OwnerNamespace, observer::relay};
use agentchat_runtime::project::ProjectManager;
use input::Callback;
use serde_json::{Value, json};
use std::path::Path;
use transport::required;

pub(super) async fn run(executable: &Path) -> Result<Value> {
    // Reject ordinary shells/wrong parents before input waits or state creation.
    let manager = ProjectManager::from_environment()?;
    let owner = crate::native::owner(manager.home(), executable, true).await?;
    let input: Callback =
        serde_json::from_slice(&relay::read_input(65_536).await?).map_err(|_| {
            Error::new(
                ErrorCode::InvalidArgument,
                "The native callback has an invalid format.",
            )
        })?;
    input.validate()?;
    let ns = OwnerNamespace::open(manager.home(), owner).await?;
    match input {
        Callback::Reserve(facts) => {
            if !facts.cwd.is_absolute() {
                return Err(required());
            }
            let cwd = tokio::fs::canonicalize(facts.cwd)
                .await
                .map_err(|_| required())?;
            let endpoint = transport::endpoint(&ns).await?.map(|e| e.nonce);
            let (reservation, prior) = ns
                .reserve_after_with_prior(facts.after_generation, facts.root, cwd, endpoint)
                .await?;
            lifecycle::root_changed(&manager, prior.clone(), facts.root)?;
            if let Some(prior) = prior {
                let path = ns.directory().to_owned();
                let owner = ns.owner().clone();
                let id = ns.id();
                let home = manager.home().to_owned();
                ns.clean_predecessor(reservation.clone(), prior, move |prior| {
                    lifecycle::remove_projection(&path, id, &prior, &owner)?;
                    if let agentchat_hosts::claude::NativeLifecycle::Project(scope) =
                        prior.lifecycle
                    {
                        agentchat_hosts::claude::routing::remove(
                            &home,
                            &agentchat_hosts::claude::observer::RegisteredOwner {
                                registration: scope,
                                owner,
                            },
                        )?;
                    }
                    Ok(())
                })
                .await?;
            }
            Ok(json!({"reservation":reservation,"owner_root":ns.id(),"observer":ns.observer()}))
        }
        Callback::Register(input) => Ok(
            json!({"registration":lifecycle::register(&manager,&ns,input.reservation,&input.version).await?}),
        ),
        Callback::Check(input) => {
            Ok(json!({"needed":lifecycle::needed(&manager,&ns,input).await?}))
        }
        Callback::Observation(scope) => Ok(serde_json::to_value(
            ns.observation_sequence(scope.root, scope.generation)
                .await?,
        )
        .map_err(|_| required())?),
        Callback::Record(input) => {
            let registration =
                lifecycle::current(&ns, input.scope.root, input.scope.generation).await?;
            let call = ns
                .prepare_call(
                    input.scope.root,
                    input.scope.generation,
                    input.tool_name,
                    input.tool_use_id,
                )
                .await?;
            // Exact pending observations survive helper retry; consumed native
            // proofs still refuse replay in the physical gateway's ledger.
            transport::send(
                &ns,
                Control::Record {
                    registration: Box::new(registration),
                    call,
                },
                false,
            )
            .await?;
            Ok(json!({"accepted":true}))
        }
        Callback::Revoke(input) => {
            ns.forget_call(
                input.scope.root,
                input.scope.generation,
                input.tool_use_id.clone(),
            )
            .await?;
            transport::send(
                &ns,
                Control::Revoke {
                    observer: ns.observer().into(),
                    root: input.scope.root,
                    generation: input.scope.generation,
                    tool_use_id: input.tool_use_id,
                },
                true,
            )
            .await?;
            Ok(json!({"accepted":true}))
        }
        Callback::Refresh(scope) => {
            lifecycle::current(&ns, scope.root, scope.generation).await?;
            transport::send(
                &ns,
                Control::Refresh {
                    observer: ns.observer().into(),
                    root: scope.root,
                    generation: scope.generation,
                },
                false,
            )
            .await?;
            Ok(json!({"accepted":true}))
        }
        Callback::Retire(scope) => {
            let path = ns.directory().to_owned();
            let owner = ns.owner().clone();
            let id = ns.id();
            let home = manager.home().to_owned();
            let retired = ns
                .retire_with(scope.root, scope.generation, move |c| {
                    lifecycle::remove_projection(&path, id, &c, &owner)?;
                    if let agentchat_hosts::claude::NativeLifecycle::Project(scope) = &c.lifecycle {
                        agentchat_hosts::claude::routing::remove(
                            &home,
                            &agentchat_hosts::claude::observer::RegisteredOwner {
                                registration: scope.clone(),
                                owner: owner.clone(),
                            },
                        )?;
                    }
                    Ok(())
                })
                .await?;
            transport::send(
                &ns,
                Control::Retire {
                    observer: ns.observer().into(),
                    root: scope.root,
                    generation: scope.generation,
                },
                true,
            )
            .await?;
            Ok(json!({"retired":retired}))
        }
    }
}
