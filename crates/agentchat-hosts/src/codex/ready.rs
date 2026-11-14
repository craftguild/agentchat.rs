//! Native readiness facts for an explicitly selected direct launch.
use super::required;
use agentchat_core::{Error, ErrorCode, Result, model::Phase, ports::HostState};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Exact native startup facts, separate from current owner proof or membership.
/// The real CLI connects directly; this never intercepts a native resume reply.
#[derive(Debug, Clone)]
pub struct CodexReady {
    root: String,
    directory: PathBuf,
    version: String,
    state: HostState,
}
impl CodexReady {
    pub(super) fn decode(
        root: &str,
        directory: &Path,
        version: &str,
        value: Value,
        state: HostState,
    ) -> Result<Self> {
        let ready = Self::decode_observed(root, version, value, state)?;
        if ready.directory != directory {
            return Err(waiting());
        }
        Ok(ready)
    }
    pub(super) fn decode_observed(
        root: &str,
        version: &str,
        value: Value,
        state: HostState,
    ) -> Result<Self> {
        if value.pointer("/thread/id").and_then(Value::as_str) != Some(root) {
            return Err(required());
        }
        let directory = value
            .pointer("/thread/cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|directory| directory.is_absolute())
            .ok_or_else(super::invalid)?;
        if state.phase == Phase::Disconnected {
            return Err(waiting());
        }
        Ok(Self {
            root: root.into(),
            directory,
            version: version.into(),
            state,
        })
    }
    /// Explicit native UUID selected before launching this designated CLI.
    pub fn root(&self) -> &str {
        &self.root
    }
    /// Native-reported directory, compared with the selected canonical project.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    /// Live initialized server version, independently matched with the image.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Startup phase only; later operations must obtain fresh native facts.
    pub fn state(&self) -> &HostState {
        &self.state
    }
}
pub(super) fn waiting() -> Error {
    let mut error = Error::new(
        ErrorCode::Busy,
        "The selected native Codex startup is not ready.",
    );
    error.retryable = true;
    error
}
