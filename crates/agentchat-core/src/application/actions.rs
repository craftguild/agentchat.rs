//! Shared message, channel, read and reservation policy for either actor kind.
use super::{Mutation, Work, messages, participation, reservations};
use crate::Result;
use serde_json::Value;
use uuid::Uuid;
pub(super) async fn apply(work: &mut Work<'_>, actor: Uuid, command: &Mutation) -> Result<Value> {
    Ok(match command {
        Mutation::ChannelCreate { name } => {
            participation::create_channel(work, name, Some(actor)).await?
        }
        Mutation::ChannelJoin { name } => participation::join_channel(work, actor, name).await?,
        Mutation::ChannelLeave { name } => participation::leave_channel(work, actor, name).await?,
        Mutation::Post { channel, body } => {
            messages::post(work, actor, channel.as_deref(), None, body).await?
        }
        Mutation::Reply {
            root_message_id,
            body,
            read_message_ids,
        } => {
            // Both operations share this mutation's savepoint and the
            // outer FULL commit. A failed reply cannot publish reads.
            let read = if read_message_ids.is_empty() {
                None
            } else {
                Some(messages::read(work, actor, read_message_ids).await?)
            };
            let mut result =
                messages::post(work, actor, None, Some(*root_message_id), body).await?;
            if let Some(read) = read {
                result["read_result"] = read;
            }
            result
        }
        Mutation::Read { message_ids } => messages::read(work, actor, message_ids).await?,
        Mutation::Reserve {
            patterns,
            ttl_seconds,
            reason,
        } => {
            reservations::acquire(
                work,
                actor,
                patterns,
                ttl_seconds.unwrap_or(600),
                reason.clone(),
            )
            .await?
        }
        Mutation::ReservationsRenew {
            reservation_id,
            generation,
        } => reservations::renew(work, actor, *reservation_id, *generation).await?,
        Mutation::ReservationsRelease {
            reservation_id,
            generation,
        } => reservations::release(work, actor, *reservation_id, *generation).await?,
        Mutation::Join { .. } | Mutation::Leave => {
            unreachable!("participation is dispatched before common actions")
        }
    })
}
