//! Validate persisted relational values before exposing domain records.

mod launch;
pub(crate) use launch::launch;

use agentchat_core::{
    Result, model::*, ports::IdempotencyRecord, sequence::Sequence, time::Timestamp,
};
use uuid::Uuid;

use crate::{entities::actor as agent, entities::*, error::integrity};

pub(crate) fn batch(row: wake_batch::Model) -> Result<agentchat_core::delivery::WakeBatch> {
    use agentchat_core::delivery::{BatchState, WakeBatch};
    let state = match row.state.as_str() {
        "pending" => BatchState::Pending,
        "inflight" => BatchState::Inflight,
        "stalled" => BatchState::Stalled,
        "settled" => BatchState::Settled,
        "cancelled" => BatchState::Cancelled,
        _ => return Err(integrity("Invalid persisted wake state.")),
    };
    let attempts: u32 = row
        .attempts
        .try_into()
        .map_err(|_| integrity("Invalid wake attempt count."))?;
    if attempts > 3
        || row
            .turn_ref
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256)
        || row
            .failure_code
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 64)
    {
        return Err(integrity("Invalid bounded wake bookkeeping."));
    }
    Ok(WakeBatch {
        id: uuid(&row.id)?,
        recipient_id: uuid(&row.recipient_id)?,
        incarnation: uuid(&row.incarnation)?,
        state,
        dispatch_ref: row.dispatch_ref.as_deref().map(uuid).transpose()?,
        turn_ref: row.turn_ref,
        attempts,
        next_eligible_at: row.next_eligible_at.as_deref().map(time).transpose()?,
        failure_code: row.failure_code,
    })
}

pub(crate) fn uuid(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|_| integrity("Invalid persisted UUID."))
}

pub(crate) fn time(value: &str) -> Result<Timestamp> {
    Timestamp::parse(value).map_err(|_| integrity("Invalid persisted UTC timestamp."))
}

pub(crate) fn seq(value: i64) -> Result<Sequence> {
    Sequence::new(value).map_err(|_| integrity("Invalid persisted event sequence."))
}

pub(crate) fn agent(row: agent::Model) -> Result<Agent> {
    actor(row)?
        .into_agent()
        .ok_or_else(|| integrity("Expected a native agent projection."))
}

pub(crate) fn actor(row: agent::Model) -> Result<Actor> {
    if row.timeout_notice_after_seq < 0 {
        return Err(integrity(
            "Invalid author notification cancellation boundary.",
        ));
    }
    let identity = match (
        row.kind.as_str(),
        row.host_kind.as_deref(),
        row.host_session_id,
    ) {
        ("agent", Some(host), Some(chat_id)) if !chat_id.is_empty() && chat_id.len() <= 1_024 => {
            ActorIdentity::Agent {
                host: match host {
                    "codex" => HostKind::Codex,
                    "claude" => HostKind::Claude,
                    _ => return Err(integrity("Invalid persisted host kind.")),
                },
                chat_id,
            }
        }
        ("user", None, None) if row.handle == "user" => ActorIdentity::User,
        _ => return Err(integrity("Invalid persisted actor identity.")),
    };
    Ok(Actor {
        actor_id: uuid(&row.id)?,
        identity,
        handle: row.handle,
        joined: match row.participation.as_str() {
            "joined" => true,
            "left" => false,
            _ => return Err(integrity("Invalid participation state.")),
        },
        first_join_at: time(&row.first_join_at)?,
        join_seq: seq(row.join_seq)?,
        leave_seq: row.leave_seq.map(seq).transpose()?,
    })
}

pub(crate) fn session(row: session::Model) -> Result<Session> {
    Ok(Session {
        incarnation: uuid(&row.incarnation)?,
        agent_id: uuid(&row.agent_id)?,
        context_id: uuid(&row.context_id)?,
        phase: match row.state.as_str() {
            "disconnected" => Phase::Disconnected,
            "idle" => Phase::Idle,
            "active" => Phase::Active,
            "approval_blocked" => Phase::ApprovalBlocked,
            "paused" => Phase::Paused,
            _ => return Err(integrity("Invalid persisted host phase.")),
        },
        heartbeat_at: time(&row.last_heartbeat)?,
        active_turn_id: row.active_turn_id,
        wake_paused: row.wake_paused,
    })
}

pub(crate) fn channel(row: channel::Model) -> Result<Channel> {
    Ok(Channel {
        id: uuid(&row.id)?,
        name: row.name,
        creator_id: row.creator_id.as_deref().map(uuid).transpose()?,
        creation_seq: seq(row.creation_seq)?,
    })
}

pub(crate) fn message(header: &Dispatch, row: message::Model) -> Result<Message> {
    let value = Message {
        id: uuid(&row.dispatch_id)?,
        channel_id: header
            .channel_id
            .ok_or_else(|| integrity("Message channel is missing."))?,
        author_id: uuid(&row.author_id)?,
        root_message_id: row.root_message_id.as_deref().map(uuid).transpose()?,
        body: row.body,
        created_at: header.created_at,
        creation_seq: header.creation_seq,
    };
    header.validate_payload(&DispatchPayload::Message(value.clone()))?;
    Ok(value)
}

pub(crate) fn dispatch(row: dispatch::Model) -> Result<Dispatch> {
    Ok(Dispatch {
        id: uuid(&row.id)?,
        kind: row.kind.parse()?,
        channel_id: row.channel_id.as_deref().map(uuid).transpose()?,
        created_at: time(&row.created_at)?,
        creation_seq: seq(row.creation_seq)?,
    })
}

pub(crate) fn notice(row: notice::Model) -> Result<DispatchNotice> {
    let value = DispatchNotice {
        dispatch_id: uuid(&row.dispatch_id)?,
        kind: row.kind.parse()?,
        data: row.data,
    };
    value.validate()?;
    Ok(value)
}

pub(crate) fn receipt(row: receipt::Model) -> Result<Receipt> {
    Ok(Receipt {
        message_id: uuid(&row.message_id)?,
        actor_id: uuid(&row.actor_id)?,
        kind: match row.kind.as_str() {
            "automatic" => ReadKind::Automatic,
            "explicit" => ReadKind::Explicit,
            _ => return Err(integrity("Invalid persisted read kind.")),
        },
        first_read_at: time(&row.first_read_at)?,
    })
}

pub(crate) fn delivery(row: delivery::Model) -> Result<Delivery> {
    let delivery = Delivery {
        id: uuid(&row.id)?,
        dispatch_id: uuid(&row.dispatch_id)?,
        recipient_id: uuid(&row.recipient_id)?,
        state: row
            .state
            .parse()
            .map_err(|_| integrity("Invalid persisted delivery state."))?,
        wake_eligible: row.wake_eligible,
        wake_batch_id: row.wake_batch_id.as_deref().map(uuid).transpose()?,
        attempts: row
            .attempts
            .try_into()
            .map_err(|_| integrity("Invalid delivery attempts."))?,
        history: DeliveryHistory {
            queued_at: time(&row.queued_at)?,
            processing_started_at: row.processing_started_at.as_deref().map(time).transpose()?,
            deadline_at: row.deadline_at.as_deref().map(time).transpose()?,
            timed_out_at: row.timed_out_at.as_deref().map(time).transpose()?,
            delivered_at: row.delivered_at.as_deref().map(time).transpose()?,
            left_at: row.left_at.as_deref().map(time).transpose()?,
        },
    };
    delivery.history.validate(delivery.state)?;
    Ok(delivery)
}

pub(crate) fn reservation(row: reservation::Model, patterns: Vec<String>) -> Result<Reservation> {
    Ok(Reservation {
        id: uuid(&row.id)?,
        owner_id: uuid(&row.owner_id)?,
        generation: row.generation,
        state: match row.state.as_str() {
            "active" => LeaseState::Active,
            "released" => LeaseState::Released,
            "expired" => LeaseState::Expired,
            _ => return Err(integrity("Invalid persisted lease state.")),
        },
        ttl_seconds: row
            .ttl_seconds
            .try_into()
            .map_err(|_| integrity("Invalid lease TTL."))?,
        acquired_at: time(&row.acquired_at)?,
        expires_at: time(&row.expires_at)?,
        released_at: row.released_at.as_deref().map(time).transpose()?,
        reason: row.reason,
        patterns,
    })
}

pub(crate) fn idempotency(row: idempotency::Model) -> Result<IdempotencyRecord> {
    Ok(IdempotencyRecord {
        request_id: uuid(&row.request_id)?,
        actor_key: row.actor_key,
        operation: row.operation,
        argument_hash: row.argument_hash,
        result: row.result,
        committed_at: time(&row.committed_at)?,
    })
}

pub(crate) fn event(row: event::Model) -> Result<Event> {
    if row.version != 1 {
        return Err(agentchat_core::Error::new(
            agentchat_core::ErrorCode::UnsupportedProtocol,
            "Stored event envelope major version is unsupported.",
        ));
    }
    let value = Event {
        version: 1,
        event_id: uuid(&row.event_id)?,
        seq: seq(row.seq)?,
        kind: row.kind.parse()?,
        occurred_at: time(&row.occurred_at)?,
        actor_id: row.actor_id.as_deref().map(uuid).transpose()?,
        channel_id: row.channel_id.as_deref().map(uuid).transpose()?,
        message_id: row.message_id.as_deref().map(uuid).transpose()?,
        data: row.data,
    };
    value.validate()?;
    Ok(value)
}

pub(crate) async fn reservation_group<C: sea_orm::ConnectionTrait>(
    db: &C,
    row: reservation::Model,
) -> Result<Reservation> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
    let patterns = reservation_pattern::Entity::find()
        .filter(reservation_pattern::Column::ReservationId.eq(&row.id))
        .order_by_asc(reservation_pattern::Column::Ordinal)
        .all(db)
        .await
        .map_err(crate::error::database)?;
    if patterns.is_empty()
        || patterns.len() > 32
        || patterns
            .iter()
            .enumerate()
            .any(|(i, p)| p.ordinal != i as i32 || p.grammar_version != 1)
    {
        return Err(integrity("Invalid persisted reservation pattern group."));
    }
    reservation(row, patterns.into_iter().map(|p| p.pattern).collect())
}
