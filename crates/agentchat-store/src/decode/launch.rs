//! Shared bounded validation for read snapshots and writer transactions.
use crate::{entities::launch, error::integrity};
use agentchat_core::{
    Result,
    model::{Launch, LaunchIdentity},
};

pub(crate) fn launch(row: launch::Model) -> Result<Launch> {
    if row.owner.is_empty()
        || row.owner.len() > 32_768
        || row.retired_at.is_some() != row.retirement_reason.is_some()
        || row
            .retirement_reason
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 64)
    {
        return Err(integrity("Invalid retained launch authority."));
    }
    let value = Launch {
        identity: LaunchIdentity {
            generation: super::uuid(&row.generation)?,
            owner: row.owner,
        },
        agent_id: super::uuid(&row.agent_id)?,
        context_id: super::uuid(&row.context_id)?,
        incarnation: super::uuid(&row.incarnation)?,
        start_seq: super::seq(row.start_seq)?,
        started_at: super::time(&row.started_at)?,
        retired_at: row.retired_at.as_deref().map(super::time).transpose()?,
        retirement_reason: row.retirement_reason,
    };
    if value.identity.generation.is_nil()
        || value.agent_id.is_nil()
        || value.context_id.is_nil()
        || value.incarnation.is_nil()
    {
        return Err(integrity("Nil retained launch identity."));
    }
    Ok(value)
}
