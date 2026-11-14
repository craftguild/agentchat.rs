//! Canonical managed-dispatch removal uses isolated roots and never invokes a host.
use agentchat_core::{ErrorCode, model::HostKind};
use agentchat_hosts::{
    install::{Installer, Remover},
    lifecycle::Lifecycle,
    setup::{Environment, Roots},
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
use uuid::Uuid;

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    environment: Environment,
    roots: Roots,
}
impl Fixture {
    async fn installed(contained: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let state = base.join("state ' quoted");
        let config = base.join("config");
        let environment = Environment {
            user_config_root: Some(config.clone()),
            claude_config_dir: contained.then(|| config.join("host profile")),
            ..Default::default()
        };
        let roots = environment.resolve().unwrap();
        for path in [
            &roots.codex_config,
            &roots.claude_registry,
            &roots.claude_settings,
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        fs::write(
            &roots.codex_config,
            "# preserve\n[mcp_servers.foreign]\ncommand='foreign'\n",
        )
        .unwrap();
        fs::write(
            &roots.claude_registry,
            json!({"mcpServers":{"foreign":{"command":"foreign"}},"foreign":true}).to_string(),
        )
        .unwrap();
        fs::write(&roots.claude_settings, json!({"permissions":{"defaultMode":"plan"},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign"}]}]}}).to_string()).unwrap();
        let executable = base.join("bin/agentchat ' literal");
        let mut artifacts = roots.codex_managed_artifacts(&executable, &state).unwrap();
        artifacts.extend(roots.claude_managed_artifacts(&executable, &state).unwrap());
        Installer::new(
            &state,
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )
        .unwrap()
        .install(artifacts)
        .await
        .unwrap();
        Self {
            _root: root,
            state,
            environment,
            roots,
        }
    }
    fn json(&self, path: &std::path::Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let remover = Remover::new(&self.state, self.environment.clone()).unwrap();
        remover
            .destinations()
            .unwrap()
            .into_iter()
            .filter(|p| p.is_file())
            .map(|p| {
                let data = fs::read(&p).unwrap();
                (p, data)
            })
            .collect()
    }
    fn assert_refused(&self, component: &str) {
        let before = self.snapshot();
        let remover = Remover::new(&self.state, self.environment.clone()).unwrap();
        let lifecycle = Lifecycle::new(&self.state).unwrap();
        let fence = lifecycle.exclusive().unwrap();
        let error = remover.prepare(Uuid::new_v4(), &fence).err().unwrap();
        assert_eq!(error.code, ErrorCode::ConfigurationConflict);
        assert_eq!(error.details["component"], component);
        for (path, data) in before {
            assert_eq!(fs::read(path).unwrap(), data);
        }
    }
    fn lose_ledger(&self) {
        fs::remove_dir_all(self.state.join("integrations")).unwrap();
    }
}

#[tokio::test]
async fn managed_dispatch_removes_only_owned_members_with_owned_lost_and_missing_state() {
    for loss in ["none", "ledger", "state"] {
        for contained in [false, true] {
            let f = Fixture::installed(contained).await;
            match loss {
                "ledger" => f.lose_ledger(),
                "state" => fs::remove_dir_all(&f.state).unwrap(),
                _ => (),
            }
            let remover = Remover::new(&f.state, f.environment.clone()).unwrap();
            let lifecycle = Lifecycle::new(&f.state).unwrap();
            let fence = lifecycle.exclusive().unwrap();
            let plan = remover.prepare(Uuid::new_v4(), &fence).unwrap();
            remover.backup(&plan, &fence).unwrap();
            for index in 0..plan.len() {
                remover.apply(&plan, index, &fence).unwrap();
            }
            remover.validate(&plan, &fence).unwrap();
            let codex = fs::read_to_string(&f.roots.codex_config).unwrap();
            let codex = codex.parse::<toml_edit::DocumentMut>().unwrap();
            assert_eq!(
                codex["mcp_servers"]["foreign"]["command"].as_str(),
                Some("foreign")
            );
            assert!(codex["mcp_servers"].get("agentchat").is_none());
            let registry = f.json(&f.roots.claude_registry);
            assert_eq!(
                registry,
                json!({"mcpServers":{"foreign":{"command":"foreign"}},"foreign":true})
            );
            assert_eq!(
                f.json(&f.roots.claude_settings),
                json!({"permissions":{"defaultMode":"plan"},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign"}]}],"FileChanged":[],"UserPromptSubmit":[]}})
            );
            assert!(!f.roots.claude_skill.parent().unwrap().exists());
            assert!(!f.roots.codex_skill.parent().unwrap().exists());
        }
    }
}

#[tokio::test]
async fn managed_mcp_rejects_mixed_modes_foreign_roots_and_unknown_environment() {
    for host in ["claude", "codex"] {
        for mutation in ["mode", "native", "version", "extra", "root"] {
            let f = Fixture::installed(true).await;
            if host == "claude" {
                let mut document = f.json(&f.roots.claude_registry);
                let env = &mut document["mcpServers"]["agentchat"]["env"];
                match mutation {
                    "mode" => env["AGENTCHAT_CLAUDE_MANAGED"] = json!("0"),
                    "native" => env["AGENTCHAT_CLAUDE_NATIVE_EXE"] = json!("/foreign/engine"),
                    "version" => env["AGENTCHAT_CLAUDE_NATIVE_VERSION"] = json!("2.1.312"),
                    "extra" => env["FOREIGN"] = json!("value"),
                    _ => env["AGENTCHAT_HOME"] = json!("/foreign/state"),
                }
                fs::write(&f.roots.claude_registry, document.to_string()).unwrap();
            } else {
                let mut document = fs::read_to_string(&f.roots.codex_config)
                    .unwrap()
                    .parse::<toml_edit::DocumentMut>()
                    .unwrap();
                let env = &mut document["mcp_servers"]["agentchat"]["env"];
                let (key, value) = match mutation {
                    "mode" => ("AGENTCHAT_CODEX_MANAGED", "0"),
                    "native" => ("AGENTCHAT_CODEX_NATIVE_EXE", "/foreign/engine"),
                    "version" => ("AGENTCHAT_CODEX_NATIVE_VERSION", "0.165.0"),
                    "extra" => ("FOREIGN", "value"),
                    _ => ("AGENTCHAT_HOME", "/foreign/state"),
                };
                env[key] = toml_edit::value(value);
                fs::write(&f.roots.codex_config, document.to_string()).unwrap();
            }
            f.lose_ledger();
            f.assert_refused(&format!("{host}.mcp"));
        }
    }
}

#[tokio::test]
async fn managed_modules_manifests_and_hooks_preserve_edited_or_ambiguous_content() {
    for mutation in ["module", "version", "sentinel", "manifest", "hook", "guard"] {
        let f = Fixture::installed(true).await;
        let plugin = f.roots.claude_skill.parent().unwrap();
        match mutation {
            "module" | "version" | "sentinel" => {
                let path = plugin.join("hooks/register.js");
                let text = fs::read_to_string(&path).unwrap();
                let changed = match mutation {
                    "module" => text + "\n// foreign edit\n",
                    "version" => text.replacen(
                        "let VERSION = \"managed\";",
                        "let VERSION = \"2.1.312\";",
                        1,
                    ),
                    _ => text.replacen("\"@managed\"", "\"@foreign\"", 1),
                };
                fs::write(path, changed).unwrap();
            }
            "manifest" => {
                let path = plugin.join(".claude-plugin/plugin.json");
                let mut document = f.json(&path);
                document["foreign"] = json!(true);
                fs::write(path, document.to_string()).unwrap();
            }
            _ => {
                let mut document = f.json(&f.roots.claude_settings);
                let hook = &mut document["hooks"]["UserPromptSubmit"][0]["hooks"][0];
                if mutation == "guard" {
                    hook["command"] = json!(
                        hook["command"]
                            .as_str()
                            .unwrap()
                            .replace(" internal claude-input ", " internal claude-guard ")
                    );
                } else {
                    hook["timeout"] = json!(11);
                }
                fs::write(&f.roots.claude_settings, document.to_string()).unwrap();
            }
        }
        f.lose_ledger();
        f.assert_refused(match mutation {
            "module" | "version" | "sentinel" => "claude.native.module",
            "manifest" => "claude.native.manifest",
            _ => "claude.native.final",
        });
    }
}
