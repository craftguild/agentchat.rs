//! Kind-specific audit/notification payload validation shared by storage fences.
use super::{ActorKind, EventKind, IdentityLabel, LeaseState};
use crate::{Error, ErrorCode, Result, time::Timestamp, validation};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

pub(super) fn validate(kind: EventKind, data: &Value) -> Result<()> {
    use EventKind::*;
    match kind {
        AgentJoined | AgentLeft | AgentLaunchRetired => {
            shape(data, &["agent_id", "handle"])?;
            id(data, "agent_id")?;
            handle(data, "handle")?;
        }
        UserJoined | UserLeft => {
            shape(data, &["actor_id", "kind", "handle"])?;
            id(data, "actor_id")?;
            require(text(data, "kind")? == "user" && text(data, "handle")? == "user")?;
        }
        ChannelCreated => {
            shape(data, &["name"])?;
            let name = text(data, "name")?;
            require(validation::channel(name).is_ok_and(|v| v == name))?;
        }
        ChannelJoined | ChannelLeft => {
            shape(data, &["agent_id", "channel_id"])?;
            id(data, "agent_id")?;
            id(data, "channel_id")?;
        }
        MessageCreated => message(data)?,
        MessageDeliveryConfirmationTimeout => timeout(data)?,
        ReservationAcquired | ReservationReleased | ReservationExpired => reservation(kind, data)?,
        ProjectChannelPolicyMaterialized | ProjectChannelPolicyChanged => policy(kind, data)?,
    }
    Ok(())
}

pub(super) fn validate_scope(kind: EventKind, channel: Option<Uuid>, data: &Value) -> Result<()> {
    use EventKind::*;
    require(channel.is_none_or(|id| !id.is_nil()))?;
    match kind {
        ChannelCreated | MessageCreated => require(channel.is_some()),
        ChannelJoined | ChannelLeft | MessageDeliveryConfirmationTimeout => {
            require(channel == Some(id(data, "channel_id")?))
        }
        _ => require(channel.is_none()),
    }
}

pub(super) fn id(data: &Value, key: &str) -> Result<Uuid> {
    let value: Uuid = decode(&data[key])?;
    require(!value.is_nil())?;
    Ok(value)
}

fn shape(data: &Value, fields: &[&str]) -> Result<()> {
    let object = data.as_object().ok_or_else(invalid)?;
    require(object.len() == fields.len() && fields.iter().all(|key| object.contains_key(*key)))
}

fn text<'a>(data: &'a Value, key: &str) -> Result<&'a str> {
    data[key].as_str().ok_or_else(invalid)
}

fn number(data: &Value, key: &str) -> Result<u64> {
    data[key].as_u64().ok_or_else(invalid)
}

fn decode<T: DeserializeOwned>(data: &Value) -> Result<T> {
    serde_json::from_value(data.clone()).map_err(|_| invalid())
}

fn handle(data: &Value, key: &str) -> Result<()> {
    let name = text(data, key)?;
    require(validation::handle(name).is_ok_and(|v| v == name))
}

fn identity(data: &Value) -> Result<IdentityLabel> {
    shape(data, &["actor_id", "kind", "handle"])?;
    let value: IdentityLabel = decode(data)?;
    require(!value.actor_id.is_nil())?;
    require(match value.kind {
        ActorKind::Agent => validation::handle(&value.handle).is_ok_and(|h| h == value.handle),
        ActorKind::User => value.handle == "user",
    })?;
    Ok(value)
}

fn message(data: &Value) -> Result<()> {
    shape(
        data,
        &[
            "thread_id",
            "mentioned_agent_ids",
            "preview",
            "body_bytes",
            "mention_results",
            "warnings",
        ],
    )?;
    id(data, "thread_id")?;
    let ids: Vec<Uuid> = decode(&data["mentioned_agent_ids"])?;
    require(
        ids.iter().all(|id| !id.is_nil())
            && ids.iter().collect::<std::collections::BTreeSet<_>>().len() == ids.len(),
    )?;
    require(text(data, "preview")?.chars().count() <= 512)?;
    require((1..=validation::MAX_BODY_BYTES as u64).contains(&number(data, "body_bytes")?))?;
    let mentions: Vec<crate::mentions::IndividualMention> = decode(&data["mention_results"])?;
    let warnings: Vec<crate::mentions::MentionWarning> = decode(&data["warnings"])?;
    require(
        mentions.len() <= crate::mentions::MAX_INDIVIDUAL_MENTIONS
            && warnings.len() <= mentions.len(),
    )?;
    for mention in mentions {
        require(
            validation::handle(&mention.handle).is_ok_and(|h| h == mention.handle)
                && mention.recipient_id.is_none_or(|id| !id.is_nil()),
        )?;
    }
    for warning in warnings {
        require(
            validation::handle(&warning.handle).is_ok_and(|h| h == warning.handle)
                && warning.recipient_id.is_none_or(|id| !id.is_nil()),
        )?;
        require(matches!(
            warning.code.as_str(),
            "unknown_recipient"
                | "recipient_left"
                | "recipient_offline"
                | "recipient_deferred"
                | "not_in_channel"
        ))?;
    }
    Ok(())
}

fn timeout(data: &Value) -> Result<()> {
    shape(
        data,
        &[
            "message_id",
            "thread_id",
            "channel_id",
            "author_id",
            "observed_at",
            "expired_count",
            "sample_recipients",
            "preview",
        ],
    )?;
    for key in ["message_id", "thread_id", "channel_id", "author_id"] {
        id(data, key)?;
    }
    let observed: Timestamp = decode(&data["observed_at"])?;
    let count = number(data, "expired_count")?;
    let sample = data["sample_recipients"].as_array().ok_or_else(invalid)?;
    require(
        count > 0
            && sample.len() <= 20
            && sample.len() as u64 <= count
            && text(data, "preview")?.chars().count() <= 512,
    )?;
    for row in sample {
        shape(
            row,
            &[
                "recipient",
                "delivery_id",
                "state",
                "deadline_at",
                "timed_out_at",
            ],
        )?;
        let recipient = identity(&row["recipient"])?;
        require(recipient.kind == ActorKind::Agent)?;
        id(row, "delivery_id")?;
        let state: super::DeliveryState = decode(&row["state"])?;
        require(matches!(
            state,
            super::DeliveryState::FailedTimeout | super::DeliveryState::DeliveredLate
        ))?;
        let deadline: Timestamp = decode(&row["deadline_at"])?;
        let expired: Timestamp = decode(&row["timed_out_at"])?;
        require(deadline < expired && expired <= observed)?;
    }
    Ok(())
}

fn reservation(kind: EventKind, data: &Value) -> Result<()> {
    shape(
        data,
        &[
            "reservation_id",
            "owner",
            "generation",
            "state",
            "ttl_seconds",
            "acquired_at",
            "expires_at",
            "released_at",
            "reason",
            "patterns",
        ],
    )?;
    id(data, "reservation_id")?;
    identity(&data["owner"])?;
    require(number(data, "generation")? > 0)?;
    let state: LeaseState = decode(&data["state"])?;
    require(
        state
            == match kind {
                EventKind::ReservationAcquired => LeaseState::Active,
                EventKind::ReservationReleased => LeaseState::Released,
                _ => LeaseState::Expired,
            },
    )?;
    let ttl = u32::try_from(number(data, "ttl_seconds")?).map_err(|_| invalid())?;
    validation::ttl(Some(ttl)).map_err(|_| invalid())?;
    let start: Timestamp = decode(&data["acquired_at"])?;
    let end: Timestamp = decode(&data["expires_at"])?;
    let released: Option<Timestamp> = decode(&data["released_at"])?;
    require(
        end > start
            && released.is_some() == (state == LeaseState::Released)
            && released.is_none_or(|time| time >= start),
    )?;
    let reason: Option<String> = decode(&data["reason"])?;
    require(reason.is_none_or(|v| v.len() <= validation::MAX_QUERY_BYTES))?;
    let patterns: Vec<String> = decode(&data["patterns"])?;
    require(!patterns.is_empty() && patterns.len() <= 200 && patterns.iter().all(|p| !p.is_empty()))
}

fn policy(kind: EventKind, data: &Value) -> Result<()> {
    if kind == EventKind::ProjectChannelPolicyMaterialized {
        shape(
            data,
            &[
                "default_channels",
                "primary_channel",
                "removed_legacy_memberships",
            ],
        )?;
        number(data, "removed_legacy_memberships")?;
    } else {
        shape(data, &["default_channels", "primary_channel"])?;
    }
    let names: Vec<String> = decode(&data["default_channels"])?;
    let policy = crate::project::ProjectChannelPolicy::new(names).map_err(|_| invalid())?;
    require(policy.primary() == text(data, "primary_channel")?)
}

pub(super) fn require(ok: bool) -> Result<()> {
    if ok { Ok(()) } else { Err(invalid()) }
}

fn invalid() -> Error {
    Error::new(
        ErrorCode::IntegrityError,
        "Audit or Notice payload disagrees with its typed kind.",
    )
}
