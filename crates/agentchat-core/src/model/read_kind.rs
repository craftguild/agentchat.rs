//! Independent first-read origins, never a caller-controlled receipt string.

use serde::{Deserialize, Serialize};

/// Each actor retains an independent immutable first time for each read kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadKind {
    /// Created only with correlated complete-message native acceptance.
    Automatic,
    /// Created only by the shared manual read or nonempty reply-read operation.
    Explicit,
}

impl ReadKind {
    /// Stable relational and wire spelling; declaration order is public order.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Explicit => "explicit",
        }
    }
}
