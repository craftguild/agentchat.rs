//! Startup authority, bounded owner observation and atomic backlog retirement.

use super::{Application, Committed, Work, participation};
use crate::{
    Error, ErrorCode, Result,
    model::{Context, Launch, LaunchOwnerState, LaunchState},
    ports::Transaction,
    validation,
};
use serde_json::json;
use uuid::Uuid;

impl Application {
    /// Read current local dispatch authority without mutating participation or
    /// invoking native getters. Unconfirmed is cancellation of local I/O only;
    /// it cannot authorize permanent retirement or discard pending work.
    pub async fn launch_state(&self, context: &Context) -> Result<LaunchState> {
        self.validate_context(context)?;
        let binding = self.host.launch_identity(context).await?;
        let mut snapshot = self.storage.snapshot().await?;
        let Some(agent) = snapshot
            .conversation(context.host, &context.host_session_id)
            .await?
            .filter(|a| a.joined)
        else {
            return Ok(LaunchState::Inactive);
        };
        if snapshot.session(agent.id).await?.is_none_or(|s| {
            s.context_id != context.context_id || s.incarnation != context.incarnation
        }) {
            return Ok(LaunchState::Inactive);
        }
        match (snapshot.latest_launch(agent.id).await?, binding) {
            (Some(launch), Some(binding))
                if launch.retired_at.is_none() && launch.identity == binding =>
            {
                if self.launch_root_changed(context, &binding) {
                    return Ok(LaunchState::Inactive);
                }
                Ok(
                    if self.host.launch_owner(&binding) == LaunchOwnerState::Current {
                        LaunchState::Current
                    } else {
                        LaunchState::Unconfirmed
                    },
                )
            }
            (Some(launch), _) if launch.retired_at.is_none() => Ok(LaunchState::Unconfirmed),
            (None, None) if self.host.allows_unmanaged_participation() => Ok(LaunchState::Current),
            _ => Ok(LaunchState::Inactive),
        }
    }
    /// Commit an explicit verified launch activation using the same atomic
    /// policy as Join. Managed readiness alone must never call this operation.
    /// Retrying an active identity returns its existing participant/boundary;
    /// a retired identity cannot be reopened, even while its owner is current.
    pub async fn start_launch(&self, context: &Context, name: Option<&str>) -> Result<Committed> {
        self.validate_context(context)?;
        let name = name.map(validation::handle).transpose()?;
        let identity = self
            .host
            .launch_identity(context)
            .await?
            .ok_or_else(|| super::activation::managed_required(context))?;
        let state = self.host.verify(context).await?;
        let mut tx = self.storage.begin().await?;
        let mut work = Work {
            tx: tx.as_mut(),
            now: self.clock.now(),
            last_seq: None,
        };
        let result = self
            .activate_join(&mut work, context, name.as_deref(), state, identity)
            .await?;
        let event_head = work.last_seq;
        tx.commit().await?;
        Ok(Committed { result, event_head })
    }

    pub(super) async fn authorize_participation(
        &self,
        tx: &mut dyn Transaction,
        context: &Context,
        leaving: bool,
    ) -> Result<()> {
        let binding = self.host.launch_identity(context).await?;
        let latest = if let Some(agent) = tx
            .conversation(context.host, &context.host_session_id)
            .await?
        {
            tx.latest_launch(agent.id).await?
        } else {
            None
        };
        match (latest, binding) {
            (Some(launch), Some(identity))
                if launch.identity == identity && (leaving || launch.retired_at.is_none()) =>
            {
                if leaving
                    || (self.host.launch_owner(&identity) == LaunchOwnerState::Current
                        && !self.launch_root_changed(context, &identity))
                {
                    Ok(())
                } else {
                    Err(required())
                }
            }
            (None, None) if self.host.allows_unmanaged_participation() => Ok(()),
            _ => Err(required()),
        }
    }

    /// Recheck one exact known generation. Unknown observations preserve all
    /// pending work; a late observation cannot retire a successor generation.
    pub async fn reconcile_launch(&self, generation: Uuid) -> Result<Committed> {
        let mut tx = self.storage.begin().await?;
        let mut work = Work {
            tx: tx.as_mut(),
            now: self.clock.now(),
            last_seq: None,
        };
        let retired = if let Some(launch) = work.tx.launch(generation).await? {
            if let Some(reason) = self.retirement_reason(work.tx, &launch).await? {
                retire(&mut work, launch, reason).await?
            } else {
                false
            }
        } else {
            false
        };
        let event_head = work.last_seq;
        tx.commit().await?;
        Ok(Committed {
            result: json!({"generation":generation,"retired":retired}),
            event_head,
        })
    }

    pub(super) async fn maintain_launches(&self, work: &mut Work<'_>) -> Result<usize> {
        let after = *self.launch_cursor.lock().expect("launch cursor poisoned");
        let launches = work.tx.active_launches(after).await?;
        let next = launches.last().map(|l| l.identity.generation);
        let mut count = 0;
        for launch in launches {
            if let Some(reason) = self.retirement_reason(work.tx, &launch).await? {
                count += usize::from(retire(work, launch, reason).await?);
            }
        }
        // This is only an advisory scan cursor, never authority. Failed outer
        // transactions may advance it; the next finite cycle revisits their rows.
        *self.launch_cursor.lock().expect("launch cursor poisoned") = next;
        Ok(count)
    }
    pub(super) fn launch_root_changed(
        &self,
        context: &Context,
        identity: &crate::model::LaunchIdentity,
    ) -> bool {
        self.host.launch_root_changed(
            self.project_id(),
            context.host,
            &context.host_session_id,
            identity,
        )
    }
    pub(super) fn launch_root_departed(
        &self,
        context: &Context,
        identity: &crate::model::LaunchIdentity,
    ) -> bool {
        self.host.launch_root_departed(
            self.project_id(),
            context.host,
            &context.host_session_id,
            identity,
        )
    }
    async fn retirement_reason(
        &self,
        tx: &mut dyn Transaction,
        launch: &Launch,
    ) -> Result<Option<&'static str>> {
        if launch.retired_at.is_some() {
            return Ok(None);
        }
        if self.host.launch_owner(&launch.identity) == LaunchOwnerState::Gone {
            return Ok(Some("owner_lost"));
        }
        let agent = tx.agent(launch.agent_id).await?.ok_or_else(required)?;
        Ok(self
            .host
            .launch_root_departed(
                self.project_id(),
                agent.host,
                &agent.host_session_id,
                &launch.identity,
            )
            .then_some("root_changed"))
    }
}

pub(super) async fn retire(work: &mut Work<'_>, mut launch: Launch, reason: &str) -> Result<bool> {
    if launch.retired_at.is_some()
        || work
            .tx
            .latest_launch(launch.agent_id)
            .await?
            .is_none_or(|latest| latest.identity.generation != launch.identity.generation)
    {
        return Ok(false);
    }
    launch.retired_at = Some(work.now);
    launch.retirement_reason = Some(reason.into());
    work.tx.save_launch(&launch).await?;
    let agent = work.tx.agent(launch.agent_id).await?.ok_or_else(required)?;
    participation::deactivate(work, agent, false, "agent.launch_retired").await?;
    Ok(true)
}

fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "A verified current AGENTCHAT launch generation is required.",
    )
}
