//! Concrete closed input DTOs; shared routing fields never imply participation.

use agentchat_core::{read::ChannelListCondition, sequence::Cursor};
use agentchat_protocol::operations::Target;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

macro_rules! input {
    ($name:ident,$context:ty,{$($fields:tt)*})=>{
        #[derive(Debug,Deserialize,JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct $name {
            /// Explicit project discovery path. Required before connection binding.
            pub project_path:Option<PathBuf>,
            /// Exact opaque context; cannot replace a bound conversation.
            pub context_id:$context,
            /// Semantic mutation retry UUID, otherwise generated and returned.
            pub request_id:Option<Uuid>,
            $($fields)*
        }
        impl $name {
            pub fn target(&self)->Target {Target{project_path:self.project_path.clone(),context_id:self.context_id.into(),..Target::default()}}
        }
    };
}
mod messages;
mod project;
mod reservations;
pub(crate) use messages::*;
pub(crate) use project::*;
pub(crate) use reservations::*;

input!(Plain, Option<Uuid>, {});
input!(Paged,Option<Uuid>,{
    /// Maximum page items, default 50; 1 through 200.
    #[schemars(range(min=1,max=200))]
    pub limit:Option<usize>,
    /// Nonnegative decimal i64 scan cursor, serialized as a string.
    #[serde(default)]
    #[schemars(with="String")]
    pub after:Cursor,
});
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelState {
    #[default]
    Active,
    Archived,
    All,
}
impl From<ChannelState> for ChannelListCondition {
    fn from(value: ChannelState) -> Self {
        match value {
            ChannelState::Active => Self::Active,
            ChannelState::Archived => Self::Archived,
            ChannelState::All => Self::All,
        }
    }
}
input!(ChannelList,Option<Uuid>,{
    /// Derived archive state, default active.
    #[serde(default)]
    #[schemars(default)]
    pub state:ChannelState,
    /// Maximum page items, default 50; 1 through 200.
    #[schemars(range(min=1,max=200))]
    pub limit:Option<usize>,
    /// Nonnegative decimal i64 scan cursor, serialized as a string.
    #[serde(default)]
    #[schemars(with="String")]
    pub after:Cursor,
});
input!(Join,Option<Uuid>,{
    /// Optional immutable ASCII-normalized participant handle.
    pub name:Option<String>,
});
input!(ChannelName,Option<Uuid>,{
    /// Channel slug, normalized and validated by the shared application.
    pub name:String,
});
input!(Members,Option<Uuid>,{
    /// Channel slug.
    pub name:String,
    /// Maximum page items, default 50; 1 through 200.
    #[schemars(range(min=1,max=200))]
    pub limit:Option<usize>,
    /// Decimal-string continuation cursor.
    #[serde(default)]
    #[schemars(with="String")]
    pub after:Cursor,
});
