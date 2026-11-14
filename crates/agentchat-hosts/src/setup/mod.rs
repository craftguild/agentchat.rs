//! Host configuration destinations and independently testable tool artifacts.
//!
//! Planning performs no writes, trust changes, session discovery or admission.
//! A tool plan is not a complete lifecycle/automatic-wake installation.
mod claude_profile;
mod codex;
mod codex_profile;
mod launch;
mod native;
mod program;
mod report;
mod selection;
mod tools;
mod workflow;

pub use codex::CodexLaunchPlan;
pub use codex_profile::CodexProfile;
pub use launch::ClaudeLaunchPlan;
pub use workflow::ToolSetup;

/// Resolve the executable entry from the captured user's PATH without an
/// installed engine fallback. The native daemon/image proof is separate.
pub fn codex_cli(search: Option<std::ffi::OsString>) -> Result<PathBuf> {
    codex::locate(search)
}

use agentchat_core::{Error, ErrorCode, Result};
use std::path::{Component, Path, PathBuf};

/// Captured environment inputs. Tests supply values without changing HOME or
/// CODEX_HOME; AGENTCHAT_USER_CONFIG_ROOT confines every selected host location.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    /// Native user home, required only without the isolated override.
    pub home: Option<PathBuf>,
    /// Explicit agentchat integration test/installation sandbox.
    pub user_config_root: Option<PathBuf>,
    /// Codex's own state/configuration override, independent of its skills.
    pub codex_home: Option<PathBuf>,
    /// Claude Code's configuration/data override.
    pub claude_config_dir: Option<PathBuf>,
}
impl Environment {
    /// Read native environment paths once without resolving or creating files.
    pub fn capture() -> Self {
        let path = |name| std::env::var_os(name).map(PathBuf::from);
        Self {
            home: path("HOME"),
            user_config_root: path("AGENTCHAT_USER_CONFIG_ROOT"),
            codex_home: path("CODEX_HOME"),
            claude_config_dir: path("CLAUDE_CONFIG_DIR"),
        }
    }
    /// Resolve destinations without filesystem access. An explicit Claude
    /// config directory inside the isolated root retains native discovery;
    /// ambient locations outside that root remain deliberately ignored.
    pub fn resolve(&self) -> Result<Roots> {
        let mut isolated_claude = None;
        let (codex, codex_skills, claude, registry) = if let Some(root) = &self.user_config_root {
            absolute(root)?;
            // Native CLAUDE_CONFIG_DIR puts the registry inside that directory,
            // unlike the default home-level registry. Honor only a contained
            // selection; a test override must never reach real host settings.
            let (claude, registry) = if let Some(path) = self
                .claude_config_dir
                .as_ref()
                .filter(|p| p.starts_with(root))
            {
                absolute(path)?;
                isolated_claude = Some(path.clone());
                (path.clone(), path.join(".claude.json"))
            } else {
                (root.join(".claude"), root.join(".claude.json"))
            };
            (
                root.join(".codex"),
                root.join(".agents/skills"),
                claude,
                registry,
            )
        } else {
            let home = self.home.as_deref().ok_or_else(|| {
                invalid("HOME or an explicit AGENTCHAT_USER_CONFIG_ROOT is required.")
            })?;
            absolute(home)?;
            let codex = self
                .codex_home
                .clone()
                .unwrap_or_else(|| home.join(".codex"));
            let claude = self
                .claude_config_dir
                .clone()
                .unwrap_or_else(|| home.join(".claude"));
            absolute(&codex)?;
            absolute(&claude)?;
            let registry = if self.claude_config_dir.is_some() {
                claude.join(".claude.json")
            } else {
                home.join(".claude.json")
            };
            (codex, home.join(".agents/skills"), claude, registry)
        };
        Ok(Roots {
            codex_config: codex.join("config.toml"),
            codex_hooks: codex.join("hooks.json"),
            codex_skill: codex_skills.join("agentchat/SKILL.md"),
            claude_registry: registry,
            claude_settings: claude.join("settings.json"),
            claude_skill: claude.join("skills/agentchat/SKILL.md"),
            isolated_root: self.user_config_root.clone(),
            isolated_claude,
        })
    }
}
/// Concrete user-wide destinations, never project-local host configuration.
#[derive(Debug, Clone)]
pub struct Roots {
    /// Codex user-level MCP configuration.
    pub codex_config: PathBuf,
    /// Codex hook definitions; planning does not preapprove their trust review.
    pub codex_hooks: PathBuf,
    /// Codex user skill under .agents, independent of CODEX_HOME.
    pub codex_skill: PathBuf,
    /// Claude user MCP registry, which also contains unrelated private state.
    pub claude_registry: PathBuf,
    /// Claude user settings and hook definitions.
    pub claude_settings: PathBuf,
    /// Claude user skill, respecting CLAUDE_CONFIG_DIR.
    pub claude_skill: PathBuf,
    isolated_root: Option<PathBuf>,
    isolated_claude: Option<PathBuf>,
}
pub(super) fn absolute(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    if !path.is_absolute()
        || path.as_os_str().as_bytes().contains(&0)
        || path.components().any(|c| c == Component::ParentDir)
    {
        return Err(invalid(
            "Configuration and executable paths must be absolute without parent traversal or NUL bytes.",
        ));
    }
    Ok(())
}
pub(super) fn encoded(path: &Path) -> Result<&str> {
    absolute(path)?;
    path.to_str().ok_or_else(|| {
        invalid("This host configuration format requires a UTF-8 executable/state path.")
    })
}
fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}
