//! Explicit native candidate plans use preserving ownership, not host admission.
use agentchat_core::{ErrorCode, model::HostKind};
use agentchat_hosts::{
    claude::observer::NativeProfile,
    install::{Content, InspectionState, Installer},
    setup::Environment,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};
#[tokio::test]
async fn native_plan_upgrades_owned_tools_preserves_foreign_policy_and_repeats_without_changes() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let native = config.join("native Claude");
    fs::create_dir_all(&native).unwrap();
    let roots = Environment {
        user_config_root: Some(config.clone()),
        claude_config_dir: Some(native.clone()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    let foreign = json!({"permissions":{"defaultMode":"plan","ask":["Write"]},"hooks":{"FileChanged":[{"matcher":"foreign","hooks":[{"type":"command","command":"foreign-watch"}]}],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"foreign-guard"}]}]}});
    fs::write(
        &roots.claude_settings,
        serde_json::to_vec(&foreign).unwrap(),
    )
    .unwrap();
    let state = root.path().join("state 'literal' $(data)");
    let executable = root.path().join("bin/agentchat 'literal' $(data)");
    let profile = NativeProfile {
        executable: root.path().join("claude 'literal' $(data)"),
        version: "2.1.301".into(),
    };
    let installer = Installer::new(&state, roots.tool_roots(&[HostKind::Claude])).unwrap();
    installer
        .install(
            roots
                .tool_artifacts(&[HostKind::Claude], &executable, &state)
                .unwrap(),
        )
        .await
        .unwrap();
    let artifacts = || {
        roots
            .claude_native_artifacts(&executable, &state, &profile)
            .unwrap()
    };
    assert_eq!(artifacts().len(), 9);
    assert_eq!(
        installer.install(artifacts()).await.unwrap().changed_files,
        6
    );
    let settings: Value =
        serde_json::from_slice(&fs::read(&roots.claude_settings).unwrap()).unwrap();
    assert_eq!(settings["permissions"], foreign["permissions"]);
    for event in ["FileChanged", "UserPromptSubmit"] {
        assert_eq!(settings["hooks"][event][0], foreign["hooks"][event][0]);
        assert_eq!(settings["hooks"][event].as_array().unwrap().len(), 2);
    }
    assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
    assert_eq!(
        settings["hooks"]["FileChanged"][1]["hooks"][0]["asyncRewake"],
        true
    );
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"][1]["hooks"][0]["onFailure"],
        "block"
    );
    assert!(settings.get("watchPaths").is_none());
    let registry: Value =
        serde_json::from_slice(&fs::read(&roots.claude_registry).unwrap()).unwrap();
    assert_eq!(
        registry["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_EXE"],
        profile.executable.to_str().unwrap()
    );
    assert_eq!(
        installer.install(artifacts()).await.unwrap().changed_files,
        0
    );
    assert_eq!(
        installer.inspect(artifacts()).await.unwrap().state,
        InspectionState::Healthy
    );
    let source = fs::read_to_string(
        roots
            .claude_skill
            .parent()
            .unwrap()
            .join("hooks/register.js"),
    )
    .unwrap();
    assert!(source.contains("transport_changed"));
    assert!(!source.contains("/private/var/tmp/agentchat-rust-native"));
    let plugin = roots.claude_skill.parent().unwrap();
    for (path, expected) in [
        (
            ".claude-plugin/plugin.json",
            json!({"name":"agentchat-native","version":"0.1.0","description":"AgentChat native conversation controls for Claude Code 2.1.301"}),
        ),
        ("hooks/hooks.json", json!({"modules":["./register.js"]})),
    ] {
        let value: Value = serde_json::from_slice(&fs::read(plugin.join(path)).unwrap()).unwrap();
        assert_eq!(value, expected);
    }
}
#[tokio::test]
async fn foreign_native_module_collision_preserves_all_host_preimages() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    fs::create_dir(&config).unwrap();
    let roots = Environment {
        user_config_root: Some(config),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    let state = root.path().join("state");
    let executable = root.path().join("bin/agentchat");
    let profile = NativeProfile {
        executable: root.path().join("claude"),
        version: "2.1.301".into(),
    };
    let installer = Installer::new(&state, roots.tool_roots(&[HostKind::Claude])).unwrap();
    installer
        .install(
            roots
                .tool_artifacts(&[HostKind::Claude], &executable, &state)
                .unwrap(),
        )
        .await
        .unwrap();
    let before = fs::read(&roots.claude_registry).unwrap();
    let module = roots
        .claude_skill
        .parent()
        .unwrap()
        .join("hooks/register.js");
    fs::create_dir_all(module.parent().unwrap()).unwrap();
    fs::write(&module, b"foreign native code").unwrap();
    let error = installer
        .install(
            roots
                .claude_native_artifacts(&executable, &state, &profile)
                .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigurationConflict);
    assert_eq!(fs::read(&module).unwrap(), b"foreign native code");
    assert_eq!(fs::read(&roots.claude_registry).unwrap(), before);
    assert!(!roots.claude_settings.exists());
}
#[tokio::test]
async fn rendered_hook_commands_preserve_literal_paths_through_the_host_shell() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    fs::create_dir(&config).unwrap();
    let roots = Environment {
        user_config_root: Some(config),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    let executable = root.path().join("agentchat 'literal' $(false)");
    fs::write(&executable, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let state = root.path().join("state 'literal' $(false)");
    let profile = NativeProfile {
        executable: root.path().join("claude 'literal' $(false)"),
        version: "2.1.301".into(),
    };
    let artifacts = roots
        .claude_native_artifacts(&executable, &state, &profile)
        .unwrap();
    let Content::JsonArray { value, .. } = &artifacts
        .iter()
        .find(|a| a.key == "claude.native.final")
        .unwrap()
        .content
    else {
        panic!("final hook")
    };
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", value["hooks"][0]["command"].as_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let args = out
        .stdout
        .split(|b| *b == 0)
        .filter(|v| !v.is_empty())
        .map(|v| String::from_utf8(v.to_vec()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        args,
        vec![
            "internal",
            "claude-input",
            "--mode",
            "final",
            "--state-directory",
            state.to_str().unwrap(),
            "--native-executable",
            profile.executable.to_str().unwrap()
        ]
    );
}
