//! Typed mutation inputs and semantic normalization shared by CLI and MCP.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, ErrorCode, Result, validation};

/// Business mutations. Transport handlers never replace these decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mutation {
    /// Explicit invitation to join this exact host conversation.
    Join {
        /// Optional immutable handle, ASCII-normalized before comparison.
        name: Option<String>,
    },
    /// Explicitly stop future delivery and release all owned leases.
    Leave,
    /// Create a work area without implicitly joining its creator.
    ChannelCreate {
        /// Channel slug.
        name: String,
    },
    /// Join one existing optional channel as the current participant.
    ChannelJoin {
        /// Channel slug.
        name: String,
    },
    /// Leave one existing optional channel while remaining in the project.
    ChannelLeave {
        /// Channel slug; configured defaults cannot be left individually.
        name: String,
    },
    /// Post an immutable root message.
    Post {
        /// Explicit channel slug; omission is resolved to the project primary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
        /// Submitted UTF-8 text; new messages strip boundary whitespace.
        /// Never a shell expression; request identity retains the submission.
        body: String,
    },
    /// Reply directly to a root in that root's channel.
    Reply {
        /// Exact complete bodies explicitly read, atomically before this reply.
        #[serde(
            default,
            deserialize_with = "nullable_ids",
            skip_serializing_if = "Vec::is_empty"
        )]
        read_message_ids: Vec<Uuid>,
        /// Complete root UUID.
        root_message_id: Uuid,
        /// Submitted UTF-8 text; new replies strip boundary whitespace.
        /// Internal bytes and the original request identity remain exact.
        body: String,
    },
    /// Mark exactly the specified messages read without thread cascading.
    Read {
        /// Atomic ID set, validated before any receipt transition.
        message_ids: Vec<Uuid>,
    },
    /// Acquire a complete ordered pattern group or acquire nothing.
    Reserve {
        /// Supported grammar-v1 logical relative path patterns.
        patterns: Vec<String>,
        /// Optional TTL; omission semantically equals 600 seconds.
        ttl_seconds: Option<u32>,
        /// Optional bounded explanation.
        reason: Option<String>,
    },
    /// Extend an owned active lease without reviving a terminal group.
    ReservationsRenew {
        /// Exact retained reservation group.
        reservation_id: Uuid,
        /// Required current generation.
        generation: i64,
    },
    /// Release an owned group; repeated same-generation release is idempotent.
    ReservationsRelease {
        /// Exact retained reservation group.
        reservation_id: Uuid,
        /// Required current generation.
        generation: i64,
    },
}

impl Mutation {
    /// Return canonical validated arguments used by idempotency hashing.
    /// Callers retaining a Join intent use this same policy for retry comparison;
    /// application operations validate it again before any durable mutation.
    pub fn normalize(mut self) -> Result<Self> {
        match &mut self {
            Self::Join { name } => *name = name.as_deref().map(validation::handle).transpose()?,
            Self::ChannelCreate { name }
            | Self::ChannelJoin { name }
            | Self::ChannelLeave { name } => *name = validation::channel(name)?,
            Self::Post { channel, body } => {
                *channel = channel.as_deref().map(validation::channel).transpose()?;
                validation::body(body)?;
            }
            Self::Reply {
                body,
                read_message_ids,
                ..
            } => {
                validation::body(body)?;
                if !read_message_ids.is_empty() {
                    normalize_ids(read_message_ids)?;
                }
            }
            Self::Read { message_ids } => normalize_ids(message_ids)?,
            Self::Reserve {
                patterns,
                ttl_seconds,
                reason,
            } => {
                if patterns.is_empty() {
                    return Err(Error::new(
                        ErrorCode::InvalidArgument,
                        "Supply at least one reservation pattern.",
                    ));
                }
                if patterns.len() > 32 {
                    // Count, byte, automaton and intersection limits all report
                    // the same explicit budget failure; none means disjoint.
                    return Err(Error::new(
                        ErrorCode::PatternTooComplex,
                        "Reservation acquisition exceeds 32 patterns.",
                    ));
                }
                *ttl_seconds = Some(validation::ttl(*ttl_seconds)?);
                if reason.as_ref().is_some_and(|r| r.len() > 1_024) {
                    return Err(Error::new(
                        ErrorCode::InvalidArgument,
                        "Reservation reason exceeds 1024 UTF-8 bytes.",
                    ));
                }
            }
            Self::ReservationsRenew { generation, .. }
            | Self::ReservationsRelease { generation, .. }
                if *generation <= 0 =>
            {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "Lease generation must be positive.",
                ));
            }
            _ => {}
        }
        Ok(self)
    }

    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Join { .. } => "join",
            Self::Leave => "leave",
            Self::ChannelCreate { .. } => "channel_create",
            Self::ChannelJoin { .. } => "channel_join",
            Self::ChannelLeave { .. } => "channel_leave",
            Self::Post { .. } => "post",
            Self::Reply { .. } => "reply",
            Self::Read { .. } => "read",
            Self::Reserve { .. } => "reserve",
            Self::ReservationsRenew { .. } => "reservations_renew",
            Self::ReservationsRelease { .. } => "reservations_release",
        }
    }
}

fn normalize_ids(ids: &mut Vec<Uuid>) -> Result<()> {
    validation::bulk_len(ids.len())?;
    ids.sort_unstable();
    ids.dedup();
    Ok(())
}

fn nullable_ids<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<Uuid>, D::Error> {
    Ok(Option::<Vec<Uuid>>::deserialize(deserializer)?.unwrap_or_default())
}
