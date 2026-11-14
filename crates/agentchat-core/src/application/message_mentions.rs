//! One transaction's immutable routing and individual availability diagnostics.
use super::work::Work;
use crate::{
    Result,
    mentions::{self, MentionParticipant, MentionResolution},
    model::Phase,
};
use uuid::Uuid;

pub(super) async fn resolve(
    work: &mut Work<'_>,
    actor: Uuid,
    channel: Uuid,
    body: &str,
) -> Result<MentionResolution> {
    let handles = mentions::bounded_tokens(body)?;
    let members = work.tx.channel_members(channel).await?;
    let mut snapshot = Vec::with_capacity(members.len());
    for agent in members {
        let session = work.tx.session(agent.id).await?;
        let connected = session.as_ref().is_some_and(|s| s.is_connected(work.now));
        let wake_deferred = connected
            && session
                .as_ref()
                .is_some_and(|s| s.wake_paused || !matches!(s.phase, Phase::Idle | Phase::Active));
        snapshot.push(MentionParticipant {
            id: agent.id,
            handle: agent.handle,
            joined: true,
            in_channel: true,
            connected,
            wake_deferred,
        });
    }
    for handle in &handles {
        if matches!(handle.as_str(), "all" | "channel" | "here")
            || snapshot.iter().any(|p| p.handle == *handle)
        {
            continue;
        }
        // Retained left identities keep their names. They are diagnostic only,
        // never wake recipients, memberships or newly invented participants.
        if let Some(agent) = work.tx.handle(handle).await? {
            snapshot.push(MentionParticipant {
                id: agent.id,
                handle: agent.handle,
                joined: agent.joined,
                in_channel: work.tx.is_member(channel, agent.id).await?,
                connected: false,
                wake_deferred: false,
            });
        }
    }
    Ok(mentions::resolve_tokens(handles, actor, &snapshot))
}
