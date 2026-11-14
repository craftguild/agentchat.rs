//! Eligibility, bounded hints and persisted-control integrity checks.

use crate::{
    Error, ErrorCode, Result,
    delivery::*,
    model::{Context, DeliveryState, Phase, Session},
    ports::HostState,
};
use sha2::{Digest, Sha256};

/// Bind the existing attempt token to its exact ordered reference set. Terminal
/// rows may leave unfinished selection, but cannot shrink an issued dispatch.
/// A legitimate later retry receives another token for its current references.
pub(super) fn dispatch_token(batch: &WakeBatch, deliveries: &[uuid::Uuid]) -> uuid::Uuid {
    let mut hash = Sha256::new();
    hash.update(b"agentchat/wake-dispatch/v1\0");
    hash.update(batch.id.as_bytes());
    hash.update(batch.recipient_id.as_bytes());
    hash.update(batch.incarnation.as_bytes());
    hash.update(batch.attempts.to_be_bytes());
    for delivery in deliveries {
        hash.update(delivery.as_bytes());
    }
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

pub(super) fn presentation(
    session: &Session,
    host: &HostState,
    now: crate::time::Timestamp,
    requirement: ActiveInput,
) -> Option<Presentation> {
    message_presentation(session, host, now, requirement)
}
pub(super) fn message_presentation(
    session: &Session,
    host: &HostState,
    now: crate::time::Timestamp,
    requirement: ActiveInput,
) -> Option<Presentation> {
    if !session.is_connected(now)
        || session.wake_paused
        || host.wake_paused
        || !matches!(session.phase, Phase::Idle | Phase::Active)
    {
        return None;
    }
    match host.phase {
        Phase::Idle => Some(Presentation::Idle),
        Phase::Active
            if requirement == ActiveInput::ExactTurnId && host.active_turn_id.is_none() =>
        {
            None
        }
        // Mechanism requirements cannot replace current root/control proof.
        Phase::Active => Some(Presentation::Active {
            expected_turn_id: host.active_turn_id.clone(),
        }),
        _ => None,
    }
}
pub(super) fn retry_safe(batch: &WakeBatch, presentation: &Presentation) -> bool {
    if batch.attempts == 0 {
        return true;
    }
    match presentation {
        Presentation::Idle => true,
        Presentation::Active {
            expected_turn_id: Some(current),
        } => batch
            .turn_ref
            .as_ref()
            .is_some_and(|previous| previous != current),
        Presentation::Active {
            expected_turn_id: None,
        } => false,
    }
}
pub(super) fn hint(
    context: &Context,
    agent: uuid::Uuid,
    batch: uuid::Uuid,
    items: &[WakeItem],
    pending: u64,
) -> Result<WakeHint> {
    if items.is_empty() || items.len() > MAX_WAKE_EVENTS {
        return Err(integrity("Invalid wake batch size."));
    }
    let mut events = Vec::with_capacity(items.len());
    for item in items {
        if item.delivery.recipient_id != agent
            || !item.delivery.wake_eligible
            || !matches!(
                item.delivery.state,
                DeliveryState::Pending | DeliveryState::Processing
            )
            || item.delivery.dispatch_id != item.dispatch.id
        {
            return Err(integrity("Invalid targeted wake candidate."));
        }
        let (kind, message_id) = match (&item.message, &item.notice) {
            (Some(message), None) => {
                item.dispatch
                    .validate_payload(&crate::model::DispatchPayload::Message(message.clone()))?;
                if message.author_id == agent {
                    return Err(integrity("A Message cannot wake its author."));
                }
                ("message.created", message.id)
            }
            (None, Some(notice)) => {
                item.dispatch
                    .validate_payload(&crate::model::DispatchPayload::Notice(notice.clone()))?;
                if notice.kind != crate::model::NoticeKind::MessageDeliveryConfirmationTimeout
                    || notice.data["author_id"].as_str() != Some(agent.to_string().as_str())
                {
                    return Err(integrity(
                        "Only the original author may receive a timeout wake.",
                    ));
                }
                let message_id = serde_json::from_value(notice.data["message_id"].clone())
                    .map_err(|_| integrity("Timeout wake Message reference is missing."))?;
                (notice.kind.as_str(), message_id)
            }
            _ => {
                return Err(integrity(
                    "Wake Header requires exactly one matching subtype.",
                ));
            }
        };
        events.push(WakeReference {
            kind: kind.into(),
            delivery_id: item.delivery.id,
            dispatch_id: item.dispatch.id,
            seq: item.dispatch.creation_seq,
            message_id,
        });
    }
    let preview = if let Some(message) = &items[0].message {
        message.body.as_str()
    } else {
        items[0]
            .notice
            .as_ref()
            .and_then(|notice| notice.data["preview"].as_str())
            .unwrap_or("")
    };
    let mut end = preview.len().min(512);
    while !preview.is_char_boundary(end) {
        end -= 1;
    }
    let hint = WakeHint {
        version: 1,
        project_id: context.project_id,
        context_id: context.context_id,
        incarnation: context.incarnation,
        agent_id: agent,
        batch_id: batch,
        events,
        pending_count: pending,
        preview: preview[..end].into(),
    };
    if serde_json::to_vec(&hint)
        .map_err(|_| integrity("Wake serialization failed."))?
        .len()
        > MAX_WAKE_BYTES
    {
        return Err(integrity("Wake context exceeds its 8 KiB budget."));
    }
    Ok(hint)
}
pub(super) fn valid_turn(turn: &Option<String>) -> Result<()> {
    if turn.as_ref().is_some_and(|s| s.is_empty() || s.len() > 256) {
        return Err(integrity("Invalid native turn reference."));
    }
    Ok(())
}
pub(super) fn integrity(message: &str) -> Error {
    Error::new(ErrorCode::IntegrityError, message)
}
