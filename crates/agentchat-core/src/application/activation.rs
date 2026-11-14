//! An explicit invitation atomically establishes one fresh delivery generation.
use super::{Application, Work, launch, participation};
use crate::{
    Error, ErrorCode, Result,
    model::{Context, Launch, LaunchIdentity, LaunchOwnerState},
    ports::HostState,
};
use serde_json::Value;

impl Application {
    pub(super) async fn join_authorized(
        &self,
        work: &mut Work<'_>,
        context: &Context,
        name: Option<&str>,
        state: HostState,
    ) -> Result<Value> {
        match self.host.launch_identity(context).await? {
            Some(identity) => {
                self.activate_join(work, context, name, state, identity)
                    .await
            }
            None if self.host.allows_unmanaged_participation() => {
                participation::join(work, context, name, state).await
            }
            None => Err(managed_required(context)),
        }
    }

    pub(super) async fn activate_join(
        &self,
        work: &mut Work<'_>,
        context: &Context,
        name: Option<&str>,
        state: HostState,
        identity: LaunchIdentity,
    ) -> Result<Value> {
        if identity.generation.is_nil()
            || identity.owner.is_empty()
            || identity.owner.len() > 32_768
            || self.host.launch_owner(&identity) != LaunchOwnerState::Current
            || self.launch_root_changed(context, &identity)
        {
            return Err(managed_required(context));
        }
        if let Some(old) = work.tx.launch(identity.generation).await? {
            let agent = work
                .tx
                .agent(old.agent_id)
                .await?
                .ok_or_else(|| managed_required(context))?;
            // A duplicate invitation reports its current participation. It does
            // not create a generation/boundary, replace a session or heartbeat.
            // A retired identity must first receive a fresh supervisor-published
            // intent; replaying an old successful request never reopens its row.
            if old.identity != identity
                || old.retired_at.is_some()
                || !agent.joined
                || agent.host != context.host
                || agent.host_session_id != context.host_session_id
            {
                return Err(managed_required(context));
            }
            if name.is_some_and(|n| n != agent.handle) {
                return Err(Error::new(
                    ErrorCode::IdentityConflict,
                    "A conversation's retained handle cannot be renamed.",
                ));
            }
            let session = work
                .tx
                .session(agent.id)
                .await?
                .ok_or_else(|| managed_required(context))?;
            return Ok(join_result(&agent, &old, session.context_id));
        }
        if let Some(agent) = work
            .tx
            .conversation(context.host, &context.host_session_id)
            .await?
        {
            if let Some(old) = work
                .tx
                .latest_launch(agent.id)
                .await?
                .filter(|l| l.retired_at.is_none())
            {
                let changed = self.launch_root_departed(context, &old.identity);
                if self.host.launch_owner(&old.identity) != LaunchOwnerState::Gone && !changed {
                    return Err(Error::new(
                        ErrorCode::IdentityConflict,
                        "A current or unconfirmed participation already owns this conversation.",
                    ));
                }
                launch::retire(
                    work,
                    old,
                    if changed {
                        "root_changed"
                    } else {
                        "owner_lost"
                    },
                )
                .await?;
            } else if agent.joined {
                participation::deactivate(work, agent, false, "agent.launch_retired").await?;
            }
        }
        let default_name = if name.is_none()
            && work
                .tx
                .conversation(context.host, &context.host_session_id)
                .await?
                .is_none()
        {
            self.host
                .launch_name(context)
                .await?
                .map(|name| crate::validation::handle(&name))
                .transpose()?
        } else {
            None
        };
        participation::join(work, context, name.or(default_name.as_deref()), state).await?;
        let agent = work
            .tx
            .conversation(context.host, &context.host_session_id)
            .await?
            .ok_or_else(|| managed_required(context))?;
        // Membership, its generation, initial stream and idempotency receipt
        // belong to the same outer FULL transaction. Native I/O and supervisor
        // publication have already completed outside this database writer.
        let generation = Launch {
            identity,
            agent_id: agent.id,
            context_id: context.context_id,
            incarnation: context.incarnation,
            start_seq: agent.join_seq,
            started_at: work.now,
            retired_at: None,
            retirement_reason: None,
        };
        work.tx.save_launch(&generation).await?;
        work.tx
            .save_message_stream(&crate::delivery::MessageStream::new(&generation))
            .await?;
        Ok(join_result(&agent, &generation, context.context_id))
    }
}
fn join_result(agent: &crate::model::Agent, launch: &Launch, context: uuid::Uuid) -> Value {
    let mut result = participation::join_result(agent);
    result["context_id"] = serde_json::json!(context);
    result["generation"] = serde_json::json!(launch.identity.generation);
    result
}

pub(super) fn managed_required(context: &Context) -> Error {
    let command = format!("agentchat launch {}", context.host.as_str());
    let mut error = Error::new(
        ErrorCode::SessionRequired,
        format!(
            "A current verified managed launch is required; start this conversation with {command}."
        ),
    );
    error.details =
        serde_json::json!({"reason":"managed_launch_required","required_command":command});
    error
}
