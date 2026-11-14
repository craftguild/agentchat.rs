//! Native-only callback facts, never MCP arguments or application operations.
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::claude::namespace::Reservation;
use serde::Deserialize;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(
    tag = "action",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum Callback {
    Reserve(Facts),
    Register(Registration),
    Check(Current),
    Record(Call),
    Revoke(Revocation),
    Retire(Scope),
    Refresh(Scope),
    Observation(Scope),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Facts {
    pub after_generation: u64,
    pub root: Uuid,
    pub cwd: PathBuf,
    pub version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registration {
    pub reservation: Reservation,
    pub version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Current {
    pub root: Uuid,
    pub generation: u64,
    pub cwd: PathBuf,
    pub version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scope {
    pub root: Uuid,
    pub generation: u64,
    pub version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Call {
    pub scope: Scope,
    pub tool_name: String,
    pub tool_use_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revocation {
    pub scope: Scope,
    pub tool_use_id: String,
}
impl Callback {
    pub fn validate(&self) -> Result<()> {
        let (version, root, generation) = match self {
            Self::Reserve(v) => (&v.version, v.root, 1),
            Self::Register(v) => (&v.version, v.reservation.root, v.reservation.generation),
            Self::Check(v) => (&v.version, v.root, v.generation),
            Self::Record(v) => (&v.scope.version, v.scope.root, v.scope.generation),
            Self::Revoke(v) => (&v.scope.version, v.scope.root, v.scope.generation),
            Self::Retire(v) | Self::Refresh(v) | Self::Observation(v) => {
                (&v.version, v.root, v.generation)
            }
        };
        if matches!(self,Self::Reserve(v) if v.after_generation>9_007_199_254_740_991) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "The native generation floor is invalid.",
            ));
        }
        let cwd = match self {
            Self::Reserve(v) => Some(&v.cwd),
            Self::Check(v) => Some(&v.cwd),
            _ => None,
        };
        if cwd.is_some_and(|p| !p.is_absolute() || p.as_os_str().len() > 16_384) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "The native working directory is invalid.",
            ));
        }
        if !agentchat_hosts::claude::module::candidate_version(version) {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "The native callback engine version is unsupported.",
            ));
        }
        if root.is_nil() || generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "The native callback scope is invalid.",
            ));
        }
        Ok(())
    }
}
