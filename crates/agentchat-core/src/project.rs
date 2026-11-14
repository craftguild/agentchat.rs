//! Persistent project identity, independent of paths and transport connections.

mod receipts;
pub use receipts::*;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, ErrorCode, Result, validation};

/// Effective default-channel policy retained by one project.
///
/// The first entry is the primary channel used for omitted root-post routing.
/// Construction normalizes names and removes later duplicates while preserving
/// first-occurrence order. An empty configured policy is never valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectChannelPolicy {
    ordered_defaults: Vec<String>,
}

impl<'de> Deserialize<'de> for ProjectChannelPolicy {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Stored {
            ordered_defaults: Vec<String>,
        }
        let stored = Stored::deserialize(deserializer)?;
        Self::new(stored.ordered_defaults).map_err(serde::de::Error::custom)
    }
}

impl ProjectChannelPolicy {
    /// Validate and normalize one explicitly supplied ordered default list.
    pub fn new(defaults: Vec<String>) -> Result<Self> {
        if defaults.is_empty() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Default channels must contain at least one channel.",
            ));
        }
        let mut ordered_defaults = Vec::with_capacity(defaults.len());
        for value in defaults {
            let name = validation::channel(value.trim())?;
            if !ordered_defaults.contains(&name) {
                ordered_defaults.push(name);
            }
        }
        Ok(Self { ordered_defaults })
    }

    /// Compatibility policy for a project created before defaults were stored.
    pub fn legacy() -> Self {
        Self {
            ordered_defaults: vec!["dev".into()],
        }
    }

    /// Ordered mandatory channels; the first entry is always present.
    pub fn ordered_defaults(&self) -> &[String] {
        &self.ordered_defaults
    }

    /// Standard public coordination channel and omitted-post target.
    pub fn primary(&self) -> &str {
        &self.ordered_defaults[0]
    }

    /// Whether membership in this channel is mandatory for participants.
    pub fn contains(&self, channel: &str) -> bool {
        self.ordered_defaults.iter().any(|name| name == channel)
    }
}

/// Discovery boundary selected at initialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Git common-directory identity, shared across linked worktrees.
    Repository,
    /// Canonical initialized directory and its eligible descendants.
    Directory,
}

impl Scope {
    /// Stable storage spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::Directory => "directory",
        }
    }
}

/// Expected identity when opening a project's isolated database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectIdentity {
    /// Random persistent project UUID; never derived from a path.
    pub id: Uuid,
    /// Scope of the project reference.
    pub scope: Scope,
}
