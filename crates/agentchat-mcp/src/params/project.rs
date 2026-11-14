//! Project initialization/administration inputs, never a generic JSON executor.

use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    Repository,
    Directory,
}
impl From<Scope> for agentchat_core::project::Scope {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::Repository => Self::Repository,
            Scope::Directory => Self::Directory,
        }
    }
}
input!(Init,Option<Uuid>,{
    /// Explicit repository/directory scope, otherwise reuse/default selection.
    pub scope:Option<Scope>,
    /// Explicit ordered mandatory defaults; omission preserves/defaults policy.
    pub default_channels:Option<Vec<String>>,
    /// Automatically read complete native message bodies; null/omission preserves it.
    pub auto_read:Option<bool>,
    /// Positive whole-second processing timeout; new projects default to 3600.
    #[schemars(range(min=1,max=2592000))]
    pub delivery_confirmation_timeout_seconds:Option<u32>,
    /// Adopt a moved root only after the old canonical location disappeared.
    #[serde(default)] pub relocate:bool,
    /// Deliberately create a fresh namespace while retaining old data.
    #[serde(default)] pub new_project:bool,
});
