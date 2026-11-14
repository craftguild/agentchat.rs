//! Native artifacts selected from a discovered host observation.
use super::{Roots, encoded};
use crate::{
    claude::{module, observer::NativeProfile},
    install::{Artifact, Content},
};
use agentchat_core::{Result, model::HostKind};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};

impl Roots {
    /// Plan native module, MCP and static/final hooks through the preserving
    /// installer. The selector records the discovered executable/version so one
    /// launch cannot mix host instances; compatibility is checked from the live
    /// API surface. Planning performs no host execution or trust approval.
    pub fn claude_native_artifacts(
        &self,
        executable: &Path,
        agentchat_home: &Path,
        profile: &NativeProfile,
    ) -> Result<Vec<Artifact>> {
        self.claude_artifacts(executable, agentchat_home, Some(profile))
    }

    /// Stable owned dispatch for future managed launches. No engine directory
    /// or observed version is baked into MCP, hooks or the installed module.
    pub fn claude_managed_artifacts(
        &self,
        executable: &Path,
        agentchat_home: &Path,
    ) -> Result<Vec<Artifact>> {
        self.claude_artifacts(executable, agentchat_home, None)
    }

    fn claude_artifacts(
        &self,
        executable: &Path,
        agentchat_home: &Path,
        profile: Option<&NativeProfile>,
    ) -> Result<Vec<Artifact>> {
        let mut artifacts = self.tool_artifacts(&[HostKind::Claude], executable, agentchat_home)?;
        let native = profile
            .map(|p| encoded(&p.executable))
            .transpose()?
            .unwrap_or(crate::launch::MANAGED_CLAUDE);
        let mut environment =
            BTreeMap::from([("AGENTCHAT_HOME".into(), encoded(agentchat_home)?.into())]);
        if let Some(root) = &self.isolated_root {
            environment.insert("AGENTCHAT_USER_CONFIG_ROOT".into(), encoded(root)?.into());
        }
        if let Some(root) = &self.isolated_claude {
            environment.insert("CLAUDE_CONFIG_DIR".into(), encoded(root)?.into());
        }
        let source = match profile {
            Some(profile) => module::render(executable, profile, &environment)?,
            None => module::render_managed(executable, &environment)?,
        };
        for artifact in &mut artifacts {
            if artifact.key == "claude.mcp"
                && let Content::Json { value, .. } = &mut artifact.content
            {
                if let Some(profile) = profile {
                    value["env"]["AGENTCHAT_CLAUDE_NATIVE_EXE"] = json!(native);
                    value["env"]["AGENTCHAT_CLAUDE_NATIVE_VERSION"] = json!(profile.version);
                } else {
                    value["env"]["AGENTCHAT_CLAUDE_MANAGED"] = json!("1");
                }
            }
        }
        artifacts.push(Artifact {
            key: "claude.native.module".into(),
            destination: self
                .claude_skill
                .parent()
                .expect("resolved skill")
                .join("hooks/register.js"),
            content: Content::File {
                text: source,
                exclusive_directory: false,
            },
        });
        // A JavaScript file inside a skill does not load itself. Keep the
        // native plugin manifest and module entrypoint under the same journal
        // and ownership ledger as its MCP entry and two independent guards.
        let plugin = self.claude_skill.parent().expect("resolved skill");
        let full_input = profile.is_none_or(|p| module::has_startup_gate_source(&p.version));
        if full_input {
            artifacts.push(Artifact {
                key: "claude.native.input".into(),
                destination: plugin.join("hooks/input.js"),
                content: Content::File {
                    text: module::input_source().into(),
                    exclusive_directory: false,
                },
            });
        }
        for (key, path, value) in [
            (
                "claude.native.manifest",
                ".claude-plugin/plugin.json",
                json!({"name":"agentchat-native","version":"0.1.0", "description":profile.map(|p| format!("AgentChat native conversation controls for Claude Code {}",p.version)).unwrap_or_else(|| "AgentChat managed Claude conversation controls".into())}),
            ),
            (
                "claude.native.modules",
                "hooks/hooks.json",
                json!({"modules":["./register.js"]}),
            ),
        ] {
            artifacts.push(Artifact {
                key: key.into(),
                destination: plugin.join(path),
                content: Content::File {
                    text: format!("{value}\n"),
                    exclusive_directory: false,
                },
            });
        }
        let mut hooks = vec![
            ("notice", "FileChanged", "claude.native.file-changed"),
            ("final", "UserPromptSubmit", "claude.native.final"),
        ];
        if full_input {
            hooks.push(("notice", "Stop", "claude.native.stop"));
        }
        for (mode, event, key) in hooks {
            let helper = if full_input {
                "claude-input"
            } else {
                "claude-guard"
            };
            let command = format!(
                "{} internal {helper} --mode {mode} --state-directory {} --native-executable {}",
                quote(encoded(executable)?),
                quote(encoded(agentchat_home)?),
                quote(native)
            );
            let hook = if mode == "notice" {
                json!({"type":"command","command":command,"asyncRewake":true,"timeout":15})
            } else {
                json!({"type":"command","command":command,"timeout":10,"onFailure":"block"})
            };
            let value = if event == "FileChanged" {
                json!({"matcher":".agentchat-wakeup","hooks":[hook]})
            } else {
                json!({"hooks":[hook]})
            };
            artifacts.push(Artifact {
                key: key.into(),
                destination: self.claude_settings.clone(),
                content: Content::JsonArray {
                    path: vec!["hooks".into(), event.into()],
                    value,
                },
            });
        }
        Ok(artifacts)
    }
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
