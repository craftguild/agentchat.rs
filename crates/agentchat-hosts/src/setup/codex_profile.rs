//! Explicit native MCP selection without claiming behavioral compatibility.
use super::{Roots, encoded};
use crate::install::{Artifact, Content};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use std::path::{Path, PathBuf};

/// Installed matching real Codex executable/version. This configuration selector
/// is not proof of a live shared server, frontend, trust or compatible behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProfile {
    /// Absolute actual native image, never an npm/Node launcher.
    pub executable: PathBuf,
    /// Live native/server version selected by the installation.
    pub version: String,
}
impl CodexProfile {
    pub(super) fn validate(&self) -> Result<()> {
        encoded(&self.executable)?;
        if self.version.is_empty()
            || self.version.len() > 128
            || self.version.chars().any(char::is_control)
        {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "The selected native Codex version is invalid.",
            ));
        }
        Ok(())
    }
}
impl Roots {
    /// Stable managed MCP entry; the current CLI is resolved from PATH at use.
    /// No engine path/version is retained in the user's configuration.
    pub fn codex_managed_artifacts(&self, executable: &Path, home: &Path) -> Result<Vec<Artifact>> {
        let mut artifacts = self.tool_artifacts(&[HostKind::Codex], executable, home)?;
        for artifact in &mut artifacts {
            if artifact.key == "codex.mcp"
                && let Content::Toml { value, .. } = &mut artifact.content
            {
                value["env"]["AGENTCHAT_CODEX_MANAGED"] = serde_json::json!("1");
            }
        }
        Ok(artifacts)
    }
    /// Plan the exact native Codex MCP entry and shared skill through the
    /// preserving installer. No native execution, API-surface check or policy
    /// override occurs. Callers supply a profile only after separate live
    /// native discovery; isolated tool-only initialization supplies none.
    pub fn codex_native_artifacts(
        &self,
        executable: &Path,
        home: &Path,
        profile: &CodexProfile,
    ) -> Result<Vec<Artifact>> {
        profile.validate()?;
        let mut artifacts = self.tool_artifacts(&[HostKind::Codex], executable, home)?;
        for artifact in &mut artifacts {
            if artifact.key == "codex.mcp"
                && let Content::Toml { value, .. } = &mut artifact.content
            {
                value["env"]["AGENTCHAT_CODEX_NATIVE_EXE"] =
                    serde_json::json!(encoded(&profile.executable)?);
                value["env"]["AGENTCHAT_CODEX_NATIVE_VERSION"] = serde_json::json!(profile.version);
            }
        }
        Ok(artifacts)
    }
}
