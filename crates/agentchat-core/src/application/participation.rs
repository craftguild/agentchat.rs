//! Project participation, immutable naming and selective channel membership.

use serde_json::{Value, json};
use uuid::Uuid;

use super::work::Work;
use crate::{
    Error, ErrorCode, Result,
    model::{Agent, Channel, Context, Phase, Session},
    ports::HostState,
    project::ProjectChannelPolicy,
};

pub(super) async fn join(
    work: &mut Work<'_>,
    context: &Context,
    name: Option<&str>,
    state: HostState,
) -> Result<Value> {
    if state.phase == Phase::Disconnected {
        return Err(Error::new(
            ErrorCode::SessionRequired,
            "Join requires a verified live host conversation.",
        ));
    }
    let existing = work
        .tx
        .conversation(context.host, &context.host_session_id)
        .await?;
    if let Some(ref agent) = existing
        && name.is_some_and(|name| name != agent.handle)
    {
        return Err(Error::new(
            ErrorCode::IdentityConflict,
            "A conversation's retained handle cannot be renamed.",
        ));
    }
    if let Some(agent) = existing.as_ref().filter(|a| a.joined) {
        bind(work, context, agent.id, state).await?;
        return Ok(join_result(agent));
    }
    let handle = match &existing {
        Some(agent) => agent.handle.clone(),
        None => choose_name(work, name).await?,
    };
    let mut event = work.allocate("agent.joined").await?;
    let agent = Agent {
        id: existing.as_ref().map_or_else(Uuid::new_v4, |a| a.id),
        host: context.host,
        host_session_id: context.host_session_id.clone(),
        handle,
        joined: true,
        first_join_at: existing.as_ref().map_or(work.now, |a| a.first_join_at),
        join_seq: event.seq,
        leave_seq: existing.as_ref().and_then(|a| a.leave_seq),
    };
    work.tx.save_agent(&agent).await?;
    bind(work, context, agent.id, state).await?;
    event.actor_id = Some(agent.id);
    event.data = json!({"agent_id":agent.id,"handle":agent.handle});
    work.record_and_notify(&event, &Default::default()).await?;
    let policy = work
        .tx
        .channel_policy()
        .await?
        .unwrap_or_else(ProjectChannelPolicy::legacy);
    for name in policy.ordered_defaults() {
        let channel = work.tx.channel(name).await?.ok_or_else(|| {
            Error::new(
                ErrorCode::IntegrityError,
                "Configured default channel is missing.",
            )
        })?;
        let mut membership = work.allocate("channel.joined").await?;
        work.tx
            .add_member(channel.id, agent.id, membership.seq)
            .await?;
        membership.actor_id = Some(agent.id);
        membership.channel_id = Some(channel.id);
        membership.data = json!({"agent_id":agent.id,"channel_id":channel.id});
        work.record_and_notify(&membership, &Default::default())
            .await?;
    }
    Ok(join_result(&agent))
}

pub(super) async fn bind(
    work: &mut Work<'_>,
    ctx: &Context,
    agent: Uuid,
    state: HostState,
) -> Result<()> {
    if let Some(previous) = work.tx.session(agent).await? {
        if previous.incarnation == ctx.incarnation && previous.context_id != ctx.context_id {
            return Err(Error::new(
                ErrorCode::IdentityConflict,
                "An incarnation cannot silently change its descriptor.",
            ));
        }
        if previous.incarnation == ctx.incarnation && !previous.is_connected(work.now) {
            return Err(Error::new(
                ErrorCode::IdentityConflict,
                "Reconnect requires a fresh connection incarnation.",
            ));
        }
    }
    work.tx
        .save_session(&Session {
            incarnation: ctx.incarnation,
            agent_id: agent,
            context_id: ctx.context_id,
            phase: state.phase,
            heartbeat_at: work.now,
            active_turn_id: state.active_turn_id,
            wake_paused: state.wake_paused,
        })
        .await?;
    // Only the already-verified current incarnation may retire older work.
    // Repeated same-incarnation heartbeats preserve its attempt budget.
    work.tx
        .retire_wake_incarnations(agent, ctx.incarnation)
        .await
}

pub(super) fn join_result(agent: &Agent) -> Value {
    json!({"agent_id":agent.id,"handle":agent.handle,"participation":"joined","join_boundary":agent.join_seq})
}

pub(super) async fn leave(work: &mut Work<'_>, agent: Agent) -> Result<Value> {
    if let Some(mut launch) = work
        .tx
        .latest_launch(agent.id)
        .await?
        .filter(|l| l.retired_at.is_none())
    {
        launch.retired_at = Some(work.now);
        launch.retirement_reason = Some("explicit_leave".into());
        work.tx.save_launch(&launch).await?;
    }
    deactivate(work, agent, true, "agent.left").await
}

pub(super) async fn deactivate(
    work: &mut Work<'_>,
    mut agent: Agent,
    cancel_watches: bool,
    kind: &str,
) -> Result<Value> {
    if !agent.joined {
        if cancel_watches {
            explicit_cleanup(work, agent.id).await?;
        }
        return Ok(
            json!({"agent_id":agent.id,"participation":"left","leave_boundary":agent.leave_seq}),
        );
    }
    let mut event = work.allocate(kind).await?;
    agent.joined = false;
    agent.leave_seq = Some(event.seq);
    work.tx.save_agent(&agent).await?;
    work.tx.remove_memberships(agent.id).await?;
    work.tx.cancel_deliveries(agent.id, work.now).await?;
    if !cancel_watches && let Some(mut session) = work.tx.session(agent.id).await? {
        session.phase = Phase::Disconnected;
        work.tx.save_session(&session).await?;
    }
    event.actor_id = Some(agent.id);
    event.data = json!({"agent_id":agent.id,"handle":agent.handle});
    work.record_and_notify(&event, &Default::default()).await?;
    // Explicit leave releases leases. Physical owner loss instead stops renewal
    // and retains their existing expiry boundary, independently of inbox discard.
    if cancel_watches {
        explicit_cleanup(work, agent.id).await?;
    }
    Ok(json!({"agent_id":agent.id,"participation":"left","leave_boundary":event.seq}))
}

pub(super) async fn explicit_cleanup(work: &mut Work<'_>, agent: Uuid) -> Result<()> {
    // An explicit leave after physical retirement still cancels author timeout
    // notices and releases remaining TTLs, without creating a second exit event.
    work.tx.cancel_author_timeout_notices(agent).await?;
    let mut scan = super::reservations::Scan::default();
    while let Some(groups) = scan.next(work).await? {
        for mut group in groups {
            if group.owner_id == agent {
                super::reservations::release_group(work, &mut group).await?;
            }
        }
    }
    Ok(())
}

async fn choose_name(work: &mut Work<'_>, name: Option<&str>) -> Result<String> {
    if let Some(name) = name {
        if work.tx.handle(name).await?.is_some() {
            return Err(Error::new(
                ErrorCode::IdentityConflict,
                "This handle belongs to a retained identity.",
            ));
        }
        return Ok(name.into());
    }
    const ADJECTIVES: [&str; 5] = ["blue", "brisk", "calm", "green", "quiet"];
    const NOUNS: [&str; 5] = ["badger", "fox", "otter", "wren", "wolf"];
    for _ in 0..32 {
        let random = Uuid::new_v4();
        let b = random.as_bytes();
        let name = format!(
            "{}-{}-{:02x}{:02x}{:02x}",
            ADJECTIVES[b[0] as usize % 5],
            NOUNS[b[1] as usize % 5],
            b[2],
            b[3],
            b[4]
        );
        if work.tx.handle(&name).await?.is_none() {
            return Ok(name);
        }
    }
    Err(Error::new(
        ErrorCode::Busy,
        "Generated handle collision budget exhausted.",
    ))
}

pub(super) async fn create_channel(
    work: &mut Work<'_>,
    name: &str,
    actor: Option<Uuid>,
) -> Result<Value> {
    if let Some(channel) = work.tx.channel(name).await? {
        let members = work.tx.channel_member_count(channel.id).await?;
        return Ok(json!({"channel_id":channel.id,"name":channel.name,"membership_count":members}));
    }
    let mut event = work.allocate("channel.created").await?;
    let channel = Channel {
        id: Uuid::new_v4(),
        name: name.into(),
        creator_id: actor,
        creation_seq: event.seq,
    };
    work.tx.insert_channel(&channel).await?;
    event.actor_id = actor;
    event.channel_id = Some(channel.id);
    event.data = json!({"name":name});
    work.record_and_notify(&event, &Default::default()).await?;
    Ok(json!({"channel_id":channel.id,"name":name,"membership_count":0}))
}

pub(super) async fn join_channel(work: &mut Work<'_>, actor: Uuid, name: &str) -> Result<Value> {
    let channel = work
        .tx
        .channel(name)
        .await?
        .ok_or_else(|| super::work::not_found("Channel does not exist."))?;
    if let Some(boundary) = work.tx.membership_boundary(channel.id, actor).await? {
        return Ok(json!({
            "channel_id": channel.id,
            "name": channel.name,
            "participation": "joined",
            "join_boundary": boundary,
            "changed": false,
        }));
    }
    let mut event = work.allocate("channel.joined").await?;
    work.tx.add_member(channel.id, actor, event.seq).await?;
    event.actor_id = Some(actor);
    event.channel_id = Some(channel.id);
    event.data = json!({"agent_id":actor,"channel_id":channel.id});
    work.record_and_notify(&event, &Default::default()).await?;
    Ok(json!({
        "channel_id": channel.id,
        "name": channel.name,
        "participation": "joined",
        "join_boundary": event.seq,
        "changed": true,
    }))
}

pub(super) async fn leave_channel(work: &mut Work<'_>, actor: Uuid, name: &str) -> Result<Value> {
    let channel = work
        .tx
        .channel(name)
        .await?
        .ok_or_else(|| super::work::not_found("Channel does not exist."))?;
    let policy = work
        .tx
        .channel_policy()
        .await?
        .unwrap_or_else(ProjectChannelPolicy::legacy);
    if policy.contains(name) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "A project default channel cannot be left individually; leave the project instead.",
        ));
    }
    if !work.tx.is_member(channel.id, actor).await? {
        return Ok(json!({
            "channel_id": channel.id,
            "name": channel.name,
            "participation": "left",
            "changed": false,
        }));
    }
    let mut event = work.allocate("channel.left").await?;
    work.tx
        .cancel_channel_deliveries(channel.id, actor, work.now)
        .await?;
    work.tx.remove_member(channel.id, actor).await?;
    super::delivery::processing::retire_channel_head(work, actor, channel.id).await?;
    event.actor_id = Some(actor);
    event.channel_id = Some(channel.id);
    event.data = json!({"agent_id":actor,"channel_id":channel.id});
    work.record_and_notify(&event, &Default::default()).await?;
    Ok(json!({
        "channel_id": channel.id,
        "name": channel.name,
        "participation": "left",
        "leave_boundary": event.seq,
        "changed": true,
    }))
}

/// Ensure default channels and current participant memberships atomically.
/// Only missing memberships receive a new forward-delivery boundary; retained
/// members never get a duplicate join event or historical input replay.
pub(super) async fn ensure_default_memberships(
    work: &mut Work<'_>,
    policy: &ProjectChannelPolicy,
) -> Result<()> {
    for name in policy.ordered_defaults() {
        if work.tx.channel(name).await?.is_none() {
            create_channel(work, name, None).await?;
        }
        let channel = work.tx.channel(name).await?.ok_or_else(|| {
            Error::new(
                ErrorCode::IntegrityError,
                "Default channel creation did not retain its channel.",
            )
        })?;
        for agent in work.tx.joined_actors().await? {
            if work.tx.is_member(channel.id, agent.actor_id).await? {
                continue;
            }
            let mut membership = work.allocate("channel.joined").await?;
            work.tx
                .add_member(channel.id, agent.actor_id, membership.seq)
                .await?;
            membership.actor_id = Some(agent.actor_id);
            membership.channel_id = Some(channel.id);
            membership.data = json!({"agent_id":agent.actor_id,"channel_id":channel.id});
            work.record_and_notify(&membership, &Default::default())
                .await?;
        }
    }
    Ok(())
}

/// Remove pre-policy implicit memberships exactly once, in the same transaction
/// that materializes the first explicit project policy. Old releases enrolled
/// every participant into every channel, so a retained non-default row cannot
/// prove an intentional optional-channel join. Later policy replacements skip
/// this cutover and therefore preserve explicit memberships.
pub(super) async fn cutover_legacy_memberships(
    work: &mut Work<'_>,
    policy: &ProjectChannelPolicy,
) -> Result<u64> {
    let agents = work.tx.joined_actors().await?;
    let mut removed = 0_u64;
    for channel in work.tx.channels().await? {
        if policy.contains(&channel.name) {
            continue;
        }
        for agent in &agents {
            if work.tx.is_member(channel.id, agent.actor_id).await? {
                work.tx
                    .cancel_channel_deliveries(channel.id, agent.actor_id, work.now)
                    .await?;
                work.tx.remove_member(channel.id, agent.actor_id).await?;
                super::delivery::processing::retire_channel_head(work, agent.actor_id, channel.id)
                    .await?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}
