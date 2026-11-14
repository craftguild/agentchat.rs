//! Shared skill and stdio MCP setup, without shell interpolation or host policy.

use super::{Roots, encoded};
use crate::install::{Artifact, Content};
use agentchat_core::{Result, model::HostKind};
use serde_json::json;
use std::path::{Path, PathBuf};

impl Roots {
    /// Build only the shared skill and initially unbound MCP artifacts for
    /// explicitly selected hosts. Callers must separately supply validated
    /// lifecycle hooks before claiming a complete integration installation.
    pub fn tool_artifacts(
        &self,
        hosts: &[HostKind],
        executable: &Path,
        agentchat_home: &Path,
    ) -> Result<Vec<Artifact>> {
        let executable = encoded(executable)?;
        let mut env = json!({"AGENTCHAT_HOME": encoded(agentchat_home)?});
        if let Some(root) = &self.isolated_root {
            env["AGENTCHAT_USER_CONFIG_ROOT"] = json!(encoded(root)?);
        }
        let mut artifacts = Vec::new();
        // Repeated host selections are set membership, never duplicate ownership.
        for host in [HostKind::Codex, HostKind::Claude] {
            if !hosts.contains(&host) {
                continue;
            }
            let mut host_env = env.clone();
            if host == HostKind::Claude
                && let Some(path) = &self.isolated_claude
            {
                // Keep public MCP health/setup on the same explicitly selected
                // native sandbox even when its launcher filters ambient env.
                host_env["CLAUDE_CONFIG_DIR"] = json!(encoded(path)?);
            }
            let (prefix, destination, content, skill) = match host {
                HostKind::Codex => (
                    "codex",
                    &self.codex_config,
                    Content::Toml {
                        path: vec!["mcp_servers".into(), "agentchat".into()],
                        value: json!({"command":executable,"args":["mcp"],"env":host_env}),
                    },
                    &self.codex_skill,
                ),
                HostKind::Claude => (
                    "claude",
                    &self.claude_registry,
                    Content::Json {
                        path: vec!["mcpServers".into(), "agentchat".into()],
                        value: json!({"type":"stdio","command":executable,"args":["mcp"],"env":host_env}),
                    },
                    &self.claude_skill,
                ),
            };
            artifacts.push(Artifact {
                key: format!("{prefix}.mcp"),
                destination: destination.clone(),
                content,
            });
            artifacts.push(Artifact {
                key: format!("{prefix}.skill"),
                destination: skill.clone(),
                content: Content::File {
                    text: include_str!("../../assets/skills/agentchat/SKILL.md").into(),
                    exclusive_directory: true,
                },
            });
        }
        Ok(artifacts)
    }
    /// Allowed directory roots for tool artifacts of the selected hosts.
    /// The registry parent can be the user's home; exact artifact selectors
    /// still restrict ownership to agentchat's MCP member and exclusive skill file.
    pub fn tool_roots(&self, hosts: &[HostKind]) -> Vec<PathBuf> {
        if let Some(root) = &self.isolated_root {
            // Retain the historical public ownership namespace, independent of
            // host availability and the contained native Claude selection.
            // Installer resolves nested roots through the outer sandbox alias.
            return vec![
                root.clone(),
                root.join(".agents/skills"),
                root.join(".claude"),
                root.join(".codex"),
            ];
        }
        let mut roots = Vec::new();
        if hosts.contains(&HostKind::Codex) {
            roots.push(
                self.codex_config
                    .parent()
                    .expect("resolved config")
                    .to_owned(),
            );
            roots.push(
                self.codex_skill
                    .ancestors()
                    .nth(2)
                    .expect("resolved skills")
                    .to_owned(),
            );
        }
        if hosts.contains(&HostKind::Claude) {
            roots.push(
                self.claude_registry
                    .parent()
                    .expect("resolved registry")
                    .to_owned(),
            );
            roots.push(
                self.claude_settings
                    .parent()
                    .expect("resolved settings")
                    .to_owned(),
            );
        }
        roots.sort();
        roots.dedup();
        roots
    }
}
