//! Historical setup result captured after publication under the installer lock.

use super::{Roots, selection, workflow::lifecycle};
use crate::install::Installed;
use agentchat_core::{Result, model::HostKind};
use serde_json::{Value, json};

pub(super) fn installed(
    roots: &Roots,
    hosts: &[HostKind],
    installed: &Installed<'_>,
) -> Result<Value> {
    let codex = if hosts.contains(&HostKind::Codex) {
        selection::codex_profile(roots, installed)?
    } else {
        None
    };
    let codex_native = codex.is_some() || selection::managed_codex(roots, installed)?;
    let claude_native = hosts.contains(&HostKind::Claude) && installed.has_prefix("claude.native.");
    let claude = if claude_native {
        match selection::claude_selection(roots, installed)? {
            selection::ClaudeSelection::Fixed(profile) => Some(profile),
            // Owned dispatch is not a live engine observation. Inspection at
            // use reports the current version; never invent a version label.
            selection::ClaudeSelection::Managed => None,
        }
    } else {
        None
    };
    let os = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let codex =
        codex.filter(|p| crate::require_native_profile(HostKind::Codex, &p.version, &os).is_ok());
    let claude =
        claude.filter(|p| crate::require_native_profile(HostKind::Claude, &p.version, &os).is_ok());
    let mut states = serde_json::Map::new();
    for (host, name) in [(HostKind::Codex, "codex"), (HostKind::Claude, "claude")] {
        let present = hosts.contains(&host);
        let components = if host == HostKind::Codex && codex_native {
            vec!["mcp", "skill", "native_gateway"]
        } else if host == HostKind::Claude && claude_native {
            vec!["mcp", "skill", "native_module"]
        } else if present {
            vec!["mcp", "skill"]
        } else {
            vec![]
        };
        states.insert(
            name.into(),
            json!({"state":if present {"tools_installed"} else {"not_installed"},
            "components":components}),
        );
    }
    Ok(
        json!({"state":if hosts.is_empty() {"not_installed"} else {"partial"},
        "hosts":states,"lifecycle":lifecycle(codex.as_ref(),claude.as_ref())}),
    )
}
