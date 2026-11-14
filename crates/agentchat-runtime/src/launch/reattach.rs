//! Reconnection binds a retained launch, never starts another participation.
use super::{StartedLaunch, required};
use crate::project::{Project, ProjectManager};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Agent, Context, LaunchOwnerState},
};
use agentchat_hosts::launch::{Binding, ClaudeReady, Endpoint, NativeChild, Registry};
use agentchat_protocol::rpc::{Call, Client, success};
use uuid::Uuid;

fn same_scope(context: &Context, original: &Context) -> bool {
    !context.context_id.is_nil()
        && !context.incarnation.is_nil()
        && context.project_id == original.project_id
        && context.host == original.host
        && context.host_session_id == original.host_session_id
        && context.host_version == original.host_version
        && context.os_profile == original.os_profile
        && context.working_directory == original.working_directory
}

/// Attach an independently verified replacement connection to the original
/// launch. The caller must publish the exact original owner/context binding;
/// shared core policy independently checks its current host/launch authority.
/// This never starts/joins a generation, changes a boundary or releases input.
pub async fn reattach_launch(
    client: &mut Client,
    context: &Context,
    original: &StartedLaunch,
) -> Result<StartedLaunch> {
    if !same_scope(context, &original.context) {
        return Err(required());
    }
    let result = success(
        client
            .call(
                Uuid::new_v4(),
                Call::Attach {
                    context_id: context.context_id,
                },
            )
            .await?,
    )?;
    let agent: Agent = serde_json::from_value(result["agent"].clone()).map_err(|_| required())?;
    if !agent.joined {
        return Err(Error::new(
            ErrorCode::NotJoined,
            "Native restoration cannot rejoin a departed participant.",
        ));
    }
    if agent.id != original.agent_id
        || agent.join_seq != original.boundary
        || agent.host != original.context.host
        || agent.host_session_id != original.context.host_session_id
    {
        return Err(required());
    }
    Ok(StartedLaunch {
        context: context.clone(),
        identity: original.identity.clone(),
        agent_id: original.agent_id,
        boundary: original.boundary,
    })
}

/// Publish a fresh same-child native route, then attach its retained launch.
/// Positive root departure is permanent, even if the original root returns.
/// The historical first-start marker is immutable and is not republished here.
pub async fn reattach_claude_launch(
    manager: &ProjectManager,
    project: &Project,
    client: &mut Client,
    child: &NativeChild,
    ready: &ClaudeReady,
    original: &StartedLaunch,
) -> Result<StartedLaunch> {
    manager.validate(project).await?;
    let registry = Registry::new(manager.home());
    if project.identity().id != original.context.project_id
        || !same_scope(ready.context(), &original.context)
        || child.identity().owner != original.identity.owner
        || child.state() != LaunchOwnerState::Current
        || child
            .startup_root()
            .is_none_or(|root| root.to_string() != original.context.host_session_id)
        || registry.root_changed(
            original.context.project_id,
            original.context.host,
            &original.context.host_session_id,
            &original.identity,
        )
    {
        return Err(required());
    }
    registry.publish(&Binding::new(
        ready.context().clone(),
        original.identity.clone(),
        Endpoint::Claude,
    )?)?;
    reattach_launch(client, ready.context(), original).await
}
