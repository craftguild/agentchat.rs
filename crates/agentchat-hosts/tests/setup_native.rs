//! Native Claude discovery inside the explicit installation sandbox.
use agentchat_core::{ErrorCode, model::HostKind};
use agentchat_hosts::{
    install::{Content, Installer},
    setup::Environment,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink};

#[tokio::test]
async fn contained_native_config_preserves_policy_and_records_its_exact_mcp_environment() {
    let temp = tempfile::tempdir().unwrap();
    let sandbox = temp.path().join("sandbox");
    let native = sandbox.join("native Claude 'literal' $(data)");
    fs::create_dir_all(&native).unwrap();
    let foreign = json!({"mcpServers":{"foreign":{"command":"foreign"}},"projects":{"/foreign":{"hasTrustDialogAccepted":false}}});
    fs::write(native.join(".claude.json"), foreign.to_string()).unwrap();
    let policy = br#"{"permissions":{"defaultMode":"plan"},"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"foreign"}]}]}}"#;
    fs::write(native.join("settings.json"), policy).unwrap();
    let roots = Environment {
        user_config_root: Some(sandbox.clone()),
        claude_config_dir: Some(native.clone()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    assert_eq!(roots.claude_registry, native.join(".claude.json"));
    assert_eq!(roots.claude_settings, native.join("settings.json"));
    assert_eq!(roots.claude_skill, native.join("skills/agentchat/SKILL.md"));
    let state = temp.path().join("state");
    let binary = temp.path().join("bin/agentchat");
    let artifacts = || {
        roots
            .tool_artifacts(&[HostKind::Claude, HostKind::Codex], &binary, &state)
            .unwrap()
    };
    let installer = Installer::new(&state, roots.tool_roots(&[HostKind::Claude])).unwrap();
    assert_eq!(
        installer.install(artifacts()).await.unwrap().changed_files,
        4
    );
    let first = fs::read(&roots.claude_registry).unwrap();
    assert_eq!(
        installer.install(artifacts()).await.unwrap().changed_files,
        0
    );
    assert_eq!(fs::read(&roots.claude_registry).unwrap(), first);
    let registry: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(registry["projects"], foreign["projects"]);
    assert_eq!(
        registry["mcpServers"]["foreign"],
        foreign["mcpServers"]["foreign"]
    );
    assert_eq!(
        registry["mcpServers"]["agentchat"]["env"],
        json!({
            "AGENTCHAT_HOME":state,"AGENTCHAT_USER_CONFIG_ROOT":sandbox,"CLAUDE_CONFIG_DIR":native
        })
    );
    let plan = artifacts();
    let codex = plan.iter().find(|a| a.key == "codex.mcp").unwrap();
    let Content::Toml { value, .. } = &codex.content else {
        panic!("Codex MCP")
    };
    assert!(value["env"].get("CLAUDE_CONFIG_DIR").is_none());
    assert_eq!(fs::read(&roots.claude_settings).unwrap(), policy);
    assert!(!sandbox.join(".claude.json").exists());
    assert!(!sandbox.join(".claude").exists());
}

#[test]
fn outside_ambient_config_stays_ignored_and_contained_traversal_is_rejected() {
    let sandbox = std::path::PathBuf::from("/test-owned/config");
    for outside in ["relative", "/foreign/claude", "/test-owned/config-sibling"] {
        let roots = Environment {
            user_config_root: Some(sandbox.clone()),
            claude_config_dir: Some(outside.into()),
            ..Environment::default()
        }
        .resolve()
        .unwrap();
        assert_eq!(roots.claude_registry, sandbox.join(".claude.json"));
        assert_eq!(
            roots.claude_skill,
            sandbox.join(".claude/skills/agentchat/SKILL.md")
        );
    }
    for bad in [
        "/test-owned/config/../foreign",
        "/test-owned/config/bad\0name",
    ] {
        let error = Environment {
            user_config_root: Some(sandbox.clone()),
            claude_config_dir: Some(bad.into()),
            ..Environment::default()
        }
        .resolve()
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }
}

#[tokio::test]
async fn a_symbolic_native_directory_cannot_escape_the_selected_sandbox() {
    let temp = tempfile::tempdir().unwrap();
    let sandbox = temp.path().join("sandbox");
    let foreign = temp.path().join("foreign");
    fs::create_dir(&sandbox).unwrap();
    fs::create_dir(&foreign).unwrap();
    let original = br#"{"mcpServers":{"foreign":{"command":"unchanged"}}}"#;
    fs::write(foreign.join(".claude.json"), original).unwrap();
    let native = sandbox.join(".claude");
    symlink(&foreign, &native).unwrap();
    let roots = Environment {
        user_config_root: Some(sandbox.clone()),
        claude_config_dir: Some(native),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    assert!(roots.tool_roots(&[HostKind::Claude]).contains(&sandbox));
    let state = temp.path().join("state");
    let error = Installer::new(&state, roots.tool_roots(&[HostKind::Claude]))
        .unwrap()
        .install(
            roots
                .tool_artifacts(&[HostKind::Claude], &temp.path().join("agentchat"), &state)
                .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigurationConflict);
    assert_eq!(fs::read(foreign.join(".claude.json")).unwrap(), original);
    assert!(!foreign.join("skills").exists());
    assert!(!state.join("integrations/ownership.json").exists());
}

#[tokio::test]
async fn existing_default_sandbox_ownership_roots_remain_compatible() {
    let temp = tempfile::tempdir().unwrap();
    let sandbox = temp.path().join("sandbox");
    let state = temp.path().join("state");
    let roots = Environment {
        user_config_root: Some(sandbox.clone()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    let hosts = [HostKind::Codex, HostKind::Claude];
    let artifacts = || {
        roots
            .tool_artifacts(&hosts, &temp.path().join("agentchat"), &state)
            .unwrap()
    };
    // Exact public allowed-root namespace used before contained native layouts.
    let previous = Installer::new(
        &state,
        vec![
            sandbox.clone(),
            sandbox.join(".agents/skills"),
            sandbox.join(".claude"),
            sandbox.join(".codex"),
        ],
    )
    .unwrap();
    assert_eq!(
        previous.install(artifacts()).await.unwrap().changed_files,
        4
    );
    let current = Installer::new(&state, roots.tool_roots(&hosts)).unwrap();
    assert_eq!(current.install(artifacts()).await.unwrap().changed_files, 0);
}
