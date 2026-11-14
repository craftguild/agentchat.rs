//! Operation selection is distinct from verified native connection authority.

use serde::{Deserialize, Serialize};

use super::{Context, HostKind};
use crate::{Error, ErrorCode, Result};

/// Shared actor category; anonymous users never become a native host kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// Exact host conversation with independently verified launch authority.
    Agent,
    /// One project-common anonymous principal shared by local callers.
    User,
}

impl ActorKind {
    /// Stable relational and public spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
        }
    }
}

/// Public subject selection. This value alone establishes no native authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActorSelector {
    /// Resolve the exact published association in the selected project.
    Native {
        /// Exact lowercase host Enum, never inferred from the chat ID.
        host: HostKind,
        /// Case-sensitive opaque host chat ID, not an internal actor UUID.
        chat_id: String,
    },
    /// Select the project-common anonymous user without a native context.
    User,
}

impl<'de> Deserialize<'de> for ActorSelector {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Serde's internally tagged unit variant accepts extra map entries.
        // An empty struct variant enforces the closed public selector schema.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Native { host: HostKind, chat_id: String },
            User {},
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Native { host, chat_id } => Self::Native { host, chat_id },
            Wire::User {} => Self::User,
        })
    }
}

impl ActorSelector {
    /// Enforce the shared opaque identity bound before route resolution.
    pub fn validate(&self) -> Result<()> {
        if let Self::Native { chat_id, .. } = self
            && (chat_id.is_empty() || chat_id.len() > 1_024)
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Native chat ID must contain 1 through 1024 UTF-8 bytes.",
            ));
        }
        Ok(())
    }
}

/// Resolved operation subject. `None` belongs only to explicit observer reads.
/// Selecting `User` must never turn into a context-free all-history read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "context", rename_all = "snake_case")]
pub enum OperationActor {
    /// Exact native descriptor, still checked against current owner/generation.
    Native(Context),
    /// Anonymous participation has no fabricated host/session/incarnation.
    User,
}
