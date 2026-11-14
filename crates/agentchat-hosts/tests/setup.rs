//! Real destinations and preservation under explicit disposable roots only.
use agentchat_core::{ErrorCode, model::HostKind};
use agentchat_hosts::{
    install::{Content, Installer},
    setup::Environment,
};
use serde_json::{Value, json};
use std::{fs, os::unix::ffi::OsStringExt, path::PathBuf};

#[test]
fn defaults_keep_codex_skills_independent_of_codex_home() {
    let roots = Environment {
        home: Some("/test-owned/home".into()),
        codex_home: Some("/test-owned/custom-codex".into()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    assert_eq!(
        roots.codex_config,
        PathBuf::from("/test-owned/custom-codex/config.toml")
    );
    assert_eq!(
        roots.codex_skill,
        PathBuf::from("/test-owned/home/.agents/skills/agentchat/SKILL.md")
    );
    assert_eq!(
        roots.claude_registry,
        PathBuf::from("/test-owned/home/.claude.json")
    );
    assert_eq!(
        roots.claude_settings,
        PathBuf::from("/test-owned/home/.claude/settings.json")
    );
    let custom = Environment {
        home: Some("/test-owned/home".into()),
        claude_config_dir: Some("/test-owned/custom-claude".into()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    assert_eq!(
        custom.claude_registry,
        PathBuf::from("/test-owned/custom-claude/.claude.json")
    );
    assert_eq!(
        custom.claude_skill,
        PathBuf::from("/test-owned/custom-claude/skills/agentchat/SKILL.md")
    );
}

#[test]
fn isolated_root_overrides_even_invalid_ambient_homes_without_writes() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config");
    let roots = Environment {
        home: None,
        user_config_root: Some(config.clone()),
        codex_home: Some("".into()),
        claude_config_dir: Some("relative".into()),
    }
    .resolve()
    .unwrap();
    for path in [
        &roots.codex_config,
        &roots.codex_hooks,
        &roots.codex_skill,
        &roots.claude_registry,
        &roots.claude_settings,
        &roots.claude_skill,
    ] {
        assert!(path.starts_with(&config));
    }
    let artifacts = roots
        .tool_artifacts(
            &[HostKind::Codex, HostKind::Claude],
            &directory.path().join("bin/agentchat"),
            &directory.path().join("state"),
        )
        .unwrap();
    assert_eq!(artifacts.len(), 4);
    assert!(!config.exists());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn invalid_selected_paths_fail_without_falling_back_to_real_configuration() {
    for value in [
        "",
        "relative",
        "/test-owned/../foreign",
        "/test-owned/\0bad",
    ] {
        let env = Environment {
            user_config_root: Some(value.into()),
            ..Environment::default()
        };
        assert_eq!(env.resolve().unwrap_err().code, ErrorCode::InvalidArgument);
    }
    assert!(Environment::default().resolve().is_err());
    for codex in [true, false] {
        let mut env = Environment {
            home: Some("/test-owned/home".into()),
            ..Environment::default()
        };
        if codex {
            env.codex_home = Some("relative".into());
        } else {
            env.claude_config_dir = Some("relative".into());
        }
        assert!(env.resolve().is_err());
    }
}

#[test]
fn native_roots_are_retained_but_unrepresentable_command_paths_are_refused() {
    let native = PathBuf::from(std::ffi::OsString::from_vec(b"/test-owned/\xff".to_vec()));
    let roots = Environment {
        home: Some(native.clone()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    assert!(roots.codex_config.starts_with(&native));
    assert!(
        roots
            .tool_artifacts(
                &[HostKind::Codex],
                &native,
                &PathBuf::from("/test-owned/agentchat")
            )
            .is_err()
    );
    let isolated = Environment {
        user_config_root: Some(native),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    assert!(
        isolated
            .tool_artifacts(
                &[HostKind::Codex],
                &PathBuf::from("/test-owned/bin/agentchat"),
                &PathBuf::from("/test-owned/agentchat")
            )
            .is_err()
    );
}

#[tokio::test]
async fn product_tool_plan_preserves_foreign_policy_and_updates_owned_binary() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("configuration with spaces");
    let state = directory.path().join("state");
    fs::create_dir_all(config.join(".codex")).unwrap();
    fs::create_dir_all(config.join(".claude")).unwrap();
    fs::write(config.join(".codex/config.toml"), "# retained\nmodel = 'foreign-model'\napproval_policy = 'on-request'\n[mcp_servers.foreign]\ncommand = 'foreign-tool'\n").unwrap();
    fs::write(config.join(".claude.json"), r#"{"oauthAccount":{"private":"test-only"},"projects":{"/foreign":{"hasTrustDialogAccepted":false}},"mcpServers":{"foreign":{"command":"foreign-tool"}},"big":1234567890123456789012345678901234567890}"#).unwrap();
    let settings = br#"{"permissions":{"defaultMode":"plan"},"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"foreign-hook"}]}]}}"#;
    fs::write(config.join(".claude/settings.json"), settings).unwrap();
    let roots = Environment {
        user_config_root: Some(config.clone()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    let hosts = [HostKind::Codex, HostKind::Claude, HostKind::Codex];
    let installer = Installer::new(&state, roots.tool_roots(&hosts)).unwrap();
    let binary = directory
        .path()
        .join("bin/agentchat 'quoted' $(touch SHOULD_NOT_EXIST) `literal`\nnext");
    let plan = || roots.tool_artifacts(&hosts, &binary, &state).unwrap();
    assert_eq!(installer.install(plan()).await.unwrap().changed_files, 4);
    let first = fs::read(&roots.claude_registry).unwrap();
    assert_eq!(installer.install(plan()).await.unwrap().changed_files, 0);
    assert_eq!(fs::read(&roots.claude_registry).unwrap(), first);
    let registry: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(
        registry["big"].to_string(),
        "1234567890123456789012345678901234567890"
    );
    assert_eq!(
        registry["projects"]["/foreign"]["hasTrustDialogAccepted"],
        false
    );
    assert_eq!(registry["oauthAccount"]["private"], "test-only");
    assert_eq!(
        registry["mcpServers"]["agentchat"]["command"],
        binary.to_str().unwrap()
    );
    assert_eq!(
        registry["mcpServers"]["agentchat"]["env"],
        json!({"AGENTCHAT_HOME":state,"AGENTCHAT_USER_CONFIG_ROOT":config})
    );
    assert_eq!(registry["mcpServers"]["agentchat"]["args"], json!(["mcp"]));
    let codex = fs::read_to_string(&roots.codex_config).unwrap();
    assert!(
        codex.starts_with("# retained\nmodel = 'foreign-model'\napproval_policy = 'on-request'\n")
    );
    assert!(codex.contains("command = 'foreign-tool'"));
    assert_eq!(fs::read(&roots.claude_settings).unwrap(), settings);
    assert!(!roots.codex_hooks.exists());
    for skill in [&roots.codex_skill, &roots.claude_skill] {
        assert_eq!(
            fs::read_to_string(skill).unwrap(),
            include_str!("../assets/skills/agentchat/SKILL.md")
        );
    }
    assert!(!directory.path().join("SHOULD_NOT_EXIST").exists());
    let next = directory.path().join("bin/agentchat-new");
    assert_eq!(
        installer
            .install(roots.tool_artifacts(&hosts, &next, &state).unwrap())
            .await
            .unwrap()
            .changed_files,
        2
    );
    let new: Value = serde_json::from_slice(&fs::read(&roots.claude_registry).unwrap()).unwrap();
    assert_eq!(
        new["mcpServers"]["agentchat"]["command"],
        next.to_str().unwrap()
    );
}

#[test]
fn selections_are_explicit_unbound_and_never_install_policy_or_lifecycle_stubs() {
    let roots = Environment {
        home: Some("/test-owned/home".into()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    let artifacts = roots
        .tool_artifacts(
            &[HostKind::Claude],
            &PathBuf::from("/test-owned/bin/agentchat"),
            &PathBuf::from("/test-owned/state"),
        )
        .unwrap();
    assert_eq!(artifacts.len(), 2);
    assert!(artifacts.iter().all(|a| a.key.starts_with("claude.")));
    let Content::Json { value, .. } = &artifacts[0].content else {
        panic!("MCP object")
    };
    assert_eq!(
        *value,
        json!({"type":"stdio","command":"/test-owned/bin/agentchat","args":["mcp"],"env":{"AGENTCHAT_HOME":"/test-owned/state"}})
    );
    assert!(
        roots
            .tool_artifacts(
                &[],
                &PathBuf::from("/test-owned/agentchat"),
                &PathBuf::from("/test-owned/state")
            )
            .unwrap()
            .is_empty()
    );
}
