//! Explicit temporary state/config inputs; no native commands or real profiles.
use agentchat_core::{ErrorCode, model::HostKind};
use agentchat_hosts::{
    claude::observer::NativeProfile,
    install::{Installer, Remover},
    lifecycle::Lifecycle,
    setup::{CodexProfile, Environment, Roots},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
use uuid::Uuid;

struct Fixture {
    root: tempfile::TempDir,
    state: PathBuf,
    environment: Environment,
    roots: Roots,
    executable: PathBuf,
}
impl Fixture {
    fn new(contained: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let state = base.join("state");
        let config = base.join("config");
        let environment = Environment {
            home: None,
            user_config_root: Some(config.clone()),
            claude_config_dir: contained.then(|| config.join("native Claude")),
            ..Default::default()
        };
        let roots = environment.resolve().unwrap();
        let executable = base.join("bin/agentchat ' $literal");
        Self {
            root,
            state,
            environment,
            roots,
            executable,
        }
    }
    fn json(&self, path: &std::path::Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
    async fn install(&self) {
        fs::create_dir_all(self.roots.codex_config.parent().unwrap()).unwrap();
        fs::create_dir_all(self.roots.claude_settings.parent().unwrap()).unwrap();
        if !self.roots.codex_config.exists() {
            fs::write(&self.roots.codex_config,"# retained policy\napproval_policy = 'on-request'\n[mcp_servers.foreign]\ncommand = 'foreign-tool'\n[mcp_servers.ac]\ncommand = 'legacy-tool'\n").unwrap();
        }
        if !self.roots.claude_registry.exists() {
            fs::write(&self.roots.claude_registry,json!({"mcpServers":{"foreign":{"command":"foreign-tool"},"ac":{"command":"legacy-tool"}},"oauth":{"token":"fixture-secret-not-for-diagnostics"},"trust":{"enabled":false}}).to_string()).unwrap();
        }
        if !self.roots.claude_settings.exists() {
            fs::write(&self.roots.claude_settings,json!({"permissions":{"defaultMode":"plan"},"sandbox":{"enabled":true},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign-stop"}]}],"FileChanged":[{"matcher":"foreign","hooks":[{"type":"command","command":"foreign-changed"}]}],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"foreign-final"}]}]}}).to_string()).unwrap();
        }
        let native = self
            .root
            .path()
            .canonicalize()
            .unwrap()
            .join("native host ' $literal");
        let mut artifacts = self
            .roots
            .codex_native_artifacts(
                &self.executable,
                &self.state,
                &CodexProfile {
                    executable: native.clone(),
                    version: "fixture-native".into(),
                },
            )
            .unwrap();
        artifacts.extend(
            self.roots
                .claude_native_artifacts(
                    &self.executable,
                    &self.state,
                    &NativeProfile {
                        executable: native,
                        version: "fixture-native".into(),
                    },
                )
                .unwrap(),
        );
        Installer::new(
            &self.state,
            self.roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )
        .unwrap()
        .install(artifacts)
        .await
        .unwrap();
    }
    fn remover(&self) -> Remover {
        Remover::new(&self.state, self.environment.clone()).unwrap()
    }
    fn assert_foreign_preserved(&self) {
        let codex = fs::read_to_string(&self.roots.codex_config).unwrap();
        assert!(codex.starts_with("# retained policy\napproval_policy = 'on-request'\n"));
        let document = codex.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            document["mcp_servers"]["foreign"]["command"].as_str(),
            Some("foreign-tool")
        );
        assert_eq!(
            document["mcp_servers"]["ac"]["command"].as_str(),
            Some("legacy-tool")
        );
        assert!(document["mcp_servers"].get("agentchat").is_none());
        let registry = self.json(&self.roots.claude_registry);
        assert_eq!(registry["mcpServers"]["foreign"]["command"], "foreign-tool");
        assert_eq!(registry["mcpServers"]["ac"]["command"], "legacy-tool");
        assert!(registry["mcpServers"].get("agentchat").is_none());
        assert!(registry["oauth"]["token"] == "fixture-secret-not-for-diagnostics");
        assert_eq!(registry["trust"]["enabled"], false);
        let settings = self.json(&self.roots.claude_settings);
        assert_eq!(settings["permissions"]["defaultMode"], "plan");
        assert_eq!(settings["sandbox"]["enabled"], true);
        for (event, command) in [
            ("Stop", "foreign-stop"),
            ("FileChanged", "foreign-changed"),
            ("UserPromptSubmit", "foreign-final"),
        ] {
            assert!(
                settings["hooks"][event]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|g| g["hooks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["command"] == command))
            );
            assert!(
                !settings["hooks"][event]
                    .to_string()
                    .contains(" internal claude-input ")
            );
        }
    }
}

#[tokio::test]
async fn missing_ledger_recognizes_the_exact_pre_cutover_agentchat_skill_and_preserves_edits() {
    for edited in [false, true] {
        let f = Fixture::new(false);
        f.install().await;
        let mut prior = include_str!("fixtures/adr0025-agentchat-skill.txt").to_owned();
        if edited {
            prior.push_str("\nforeign additions must remain\n");
        }
        fs::write(&f.roots.codex_skill, &prior).unwrap();
        fs::remove_dir_all(f.state.join("integrations")).unwrap();
        let lifecycle = Lifecycle::new(&f.state).unwrap();
        let fence = lifecycle.exclusive().unwrap();
        let remover = f.remover();
        if edited {
            assert_eq!(
                remover.prepare(Uuid::new_v4(), &fence).err().unwrap().code,
                ErrorCode::ConfigurationConflict
            );
            assert_eq!(fs::read_to_string(&f.roots.codex_skill).unwrap(), prior);
        } else {
            let plan = remover.prepare(Uuid::new_v4(), &fence).unwrap();
            remover.backup(&plan, &fence).unwrap();
            for index in 0..plan.len() {
                remover.apply(&plan, index, &fence).unwrap();
            }
            assert!(!f.roots.codex_skill.exists());
            f.assert_foreign_preserved();
        }
    }
}

#[tokio::test]
async fn owned_and_lost_ledger_cleanup_preserves_foreign_controls_and_surviving_private_preimages()
{
    for loss in ["none", "ledger", "entire-state"] {
        for contained in [false, true] {
            let f = Fixture::new(contained);
            f.install().await;
            let original_registry = fs::read(&f.roots.claude_registry).unwrap();
            match loss {
                "ledger" => fs::remove_dir_all(f.state.join("integrations")).unwrap(),
                "entire-state" => fs::remove_dir_all(&f.state).unwrap(),
                _ => (),
            }
            let remover = f.remover();
            let lifecycle = Lifecycle::new(&f.state).unwrap();
            let fence = lifecycle.exclusive().unwrap();
            let plan = remover.prepare(Uuid::new_v4(), &fence).unwrap();
            assert!(!plan.is_empty());
            assert_eq!(
                fs::read(&f.roots.claude_registry).unwrap(),
                original_registry
            );
            let backup = remover.backup(&plan, &fence).unwrap().unwrap();
            assert!(!backup.starts_with(&f.state));
            assert_eq!(
                fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let preimages = backup.join("preimages.json");
            assert_eq!(
                fs::metadata(&preimages).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert!(
                fs::read_to_string(&preimages)
                    .unwrap()
                    .contains("fixture-secret-not-for-diagnostics")
            );
            for index in 0..plan.len() {
                remover.apply(&plan, index, &fence).unwrap();
            }
            remover.validate(&plan, &fence).unwrap();
            f.assert_foreign_preserved();
            assert!(!f.roots.codex_skill.parent().unwrap().exists());
            assert!(!f.roots.claude_skill.parent().unwrap().exists());
            let repeated = remover.prepare(Uuid::new_v4(), &fence).unwrap();
            assert!(repeated.is_empty());
            assert!(remover.backup(&repeated, &fence).unwrap().is_none());
            if f.state.exists() {
                fs::remove_dir_all(&f.state).unwrap();
            }
            assert!(preimages.exists());
            drop(fence);
            f.install().await;
            assert!(f.roots.codex_skill.exists());
            assert!(f.roots.claude_skill.exists());
        }
    }
}

#[tokio::test]
async fn mixed_hook_group_members_and_metadata_survive_with_or_without_ledger() {
    for lost in [false, true] {
        let f = Fixture::new(false);
        f.install().await;
        let mut settings = f.json(&f.roots.claude_settings);
        let groups = settings["hooks"]["Stop"].as_array_mut().unwrap();
        let group = groups
            .iter_mut()
            .find(|g| g["hooks"].to_string().contains("claude-input"))
            .unwrap();
        group["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"command","command":"foreign-in-mixed-group","timeout":29}));
        group["foreignNote"] = json!("keep group metadata");
        fs::write(&f.roots.claude_settings, settings.to_string()).unwrap();
        if lost {
            fs::remove_dir_all(f.state.join("integrations")).unwrap();
        }
        let remover = f.remover();
        let lifecycle = Lifecycle::new(&f.state).unwrap();
        let fence = lifecycle.exclusive().unwrap();
        let plan = remover.prepare(Uuid::new_v4(), &fence).unwrap();
        remover.backup(&plan, &fence).unwrap();
        for index in 0..plan.len() {
            remover.apply(&plan, index, &fence).unwrap();
        }
        f.assert_foreign_preserved();
        let settings = f.json(&f.roots.claude_settings);
        let group = settings["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g.get("foreignNote").is_some())
            .unwrap();
        assert_eq!(group["foreignNote"], "keep group metadata");
        assert_eq!(group["hooks"][0]["command"], "foreign-in-mixed-group");
    }
}

#[tokio::test]
async fn ambiguous_mcp_skills_modules_and_unknown_bundle_members_refuse_before_any_host_edit() {
    for variant in ["mcp", "skill", "module", "unknown-member"] {
        let f = Fixture::new(false);
        f.install().await;
        fs::remove_dir_all(f.state.join("integrations")).unwrap();
        match variant {
            "mcp" => {
                let mut registry = f.json(&f.roots.claude_registry);
                registry["mcpServers"]["agentchat"] = json!({"command":"/foreign/agentchat-looking-name","args":["serve"],"credential":"fixture-secret-not-for-diagnostics"});
                fs::write(&f.roots.claude_registry, registry.to_string()).unwrap();
            }
            "skill" => fs::write(
                &f.roots.codex_skill,
                "---\nname: agentchat\n---\nForeign user skill text.\n",
            )
            .unwrap(),
            "module" => {
                let path = f
                    .roots
                    .claude_skill
                    .parent()
                    .unwrap()
                    .join("hooks/register.js");
                let text = fs::read_to_string(&path).unwrap();
                fs::write(path, text + "\n// foreign edit\n").unwrap();
            }
            _ => fs::write(
                f.roots
                    .claude_skill
                    .parent()
                    .unwrap()
                    .join("foreign-file.txt"),
                b"keep me",
            )
            .unwrap(),
        }
        let registry = fs::read(&f.roots.claude_registry).unwrap();
        let codex = fs::read(&f.roots.codex_config).unwrap();
        let remover = f.remover();
        let lifecycle = Lifecycle::new(&f.state).unwrap();
        let fence = lifecycle.exclusive().unwrap();
        let error = remover
            .prepare(Uuid::new_v4(), &fence)
            .err()
            .expect("ambiguous component must refuse");
        assert_eq!(error.code, ErrorCode::ConfigurationConflict);
        assert!(error.details.get("component").is_some());
        assert!(!format!("{error:?}").contains("fixture-secret-not-for-diagnostics"));
        assert_eq!(fs::read(&f.roots.claude_registry).unwrap(), registry);
        assert_eq!(fs::read(&f.roots.codex_config).unwrap(), codex);
    }
}

#[tokio::test]
async fn foreign_edits_between_components_stop_retry_without_replacing_them() {
    let f = Fixture::new(false);
    f.install().await;
    let remover = f.remover();
    let lifecycle = Lifecycle::new(&f.state).unwrap();
    let fence = lifecycle.exclusive().unwrap();
    let plan = remover.prepare(Uuid::new_v4(), &fence).unwrap();
    remover.backup(&plan, &fence).unwrap();
    remover.apply(&plan, 0, &fence).unwrap();
    let mut settings = f.json(&f.roots.claude_settings);
    settings["foreignArrival"] = json!("preserve after partial cleanup");
    fs::write(&f.roots.claude_settings, settings.to_string()).unwrap();
    let before = fs::read(&f.roots.claude_settings).unwrap();
    assert_eq!(
        remover.apply(&plan, 1, &fence).unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(&f.roots.claude_settings).unwrap(), before);
    assert!(f.roots.codex_skill.exists());
    assert!(f.roots.claude_skill.exists());
}

#[tokio::test]
async fn serialized_plan_cannot_remove_unrelated_settings_or_use_another_scope_fence() {
    let f = Fixture::new(false);
    f.install().await;
    let remover = f.remover();
    let lifecycle = Lifecycle::new(&f.state).unwrap();
    let fence = lifecycle.exclusive().unwrap();
    let plan = remover.prepare(Uuid::new_v4(), &fence).unwrap();
    let mut encoded = serde_json::to_value(&plan).unwrap();
    let edit = encoded["edits"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["keys"] == json!(["claude.mcp"]))
        .unwrap();
    edit["after"] = json!("{}\n");
    let forged: agentchat_hosts::install::RemovalPlan = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        remover.validate(&forged, &fence).unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    let other = Lifecycle::new(&f.root.path().canonicalize().unwrap().join("other-state")).unwrap();
    let other_fence = other.exclusive().unwrap();
    assert_eq!(
        remover
            .prepare(Uuid::new_v4(), &other_fence)
            .err()
            .unwrap()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert!(
        f.json(&f.roots.claude_registry)["mcpServers"]
            .get("agentchat")
            .is_some()
    );
}
