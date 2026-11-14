//! Every destination is under a disposable explicit state/configuration root.
#![allow(dead_code)]
use agentchat_hosts::install::{Artifact, Content, Installer};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub struct Fixture {
    pub root: tempfile::TempDir,
    pub state: PathBuf,
    pub config: PathBuf,
    pub installer: Installer,
}
impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("agentchat");
        let config = root.path().join("config");
        fs::create_dir(&config).unwrap();
        let installer = Installer::new(&state, vec![config.clone()]).unwrap();
        Self {
            root,
            state,
            config,
            installer,
        }
    }
    pub fn seed(&self) {
        fs::create_dir(self.config.join(".codex")).unwrap();
        fs::create_dir(self.config.join(".claude")).unwrap();
        fs::write(self.config.join(".codex/config.toml"),"# Keep this comment exactly\nmodel = 'foreign-model' # keep model\n\n[mcp_servers.foreign]\ncommand = 'foreign-tool' # keep command\n").unwrap();
        fs::write(self.config.join(".claude.json"),r#"{"oauthAccount":{"redacted":"test-only"},"projects":{"/foreign":{"hasTrustDialogAccepted":false}},"mcpServers":{"foreign":{"command":"foreign-tool"}},"big":1234567890123456789012345678901234567890}"#).unwrap();
        fs::write(self.config.join(".claude/settings.json"),serde_json::to_vec_pretty(&json!({"permissions":{"defaultMode":"plan","deny":["Read(.env)"]},"watchPaths":["foreign.log"],"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"foreign-hook"}]}]}})).unwrap()).unwrap();
    }
    pub fn artifacts(&self, version: u32) -> Vec<Artifact> {
        artifacts(&self.config, version)
    }
    pub fn json(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.config.join(name)).unwrap()).unwrap()
    }
}
pub fn artifacts(config: &Path, version: u32) -> Vec<Artifact> {
    let command = format!("/test-owned/bin/agentchat-{version}");
    vec![
        Artifact {
            key: "codex.mcp".into(),
            destination: config.join(".codex/config.toml"),
            content: Content::Toml {
                path: vec!["mcp_servers".into(), "agentchat".into()],
                value: json!({"command":command,"args":["mcp"],"startup_timeout_sec":10}),
            },
        },
        Artifact {
            key: "claude.mcp".into(),
            destination: config.join(".claude.json"),
            content: Content::Json {
                path: vec!["mcpServers".into(), "agentchat".into()],
                value: json!({"type":"stdio","command":command,"args":["mcp"]}),
            },
        },
        Artifact {
            key: "claude.start".into(),
            destination: config.join(".claude/settings.json"),
            content: Content::JsonArray {
                path: vec!["hooks".into(), "SessionStart".into()],
                value: json!({"hooks":[{"type":"command","command":format!("'{command}' internal hook --host claude --event start")}]}),
            },
        },
        Artifact {
            key: "codex.skill".into(),
            destination: config.join(".agents/skills/agentchat/SKILL.md"),
            content: Content::File {
                text: format!(
                    "---\nname: agentchat\ndescription: Test-owned coordination fixture.\n---\nTest fixture version {version}.\n"
                ),
                exclusive_directory: true,
            },
        },
    ]
}
