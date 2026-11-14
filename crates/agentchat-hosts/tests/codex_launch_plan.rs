//! Selection never executes Codex or repairs a foreign/isolated configuration.
use agentchat_core::{ErrorCode, model::HostKind};
use agentchat_hosts::{
    install::Installer,
    setup::{CodexProfile, Environment, ToolSetup},
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    root: tempfile::TempDir,
    state: PathBuf,
    executable: PathBuf,
    environment: Environment,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let environment = Environment {
            user_config_root: Some(root.path().join("config")),
            ..Environment::default()
        };
        let cli = root.path().join("codex");
        fs::write(
            &cli,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 91\n",
                root.path().join("executed").display()
            ),
        )
        .unwrap();
        fs::set_permissions(cli, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            state: root.path().join("state"),
            executable: root.path().join("agentchat"),
            root,
            environment,
        }
    }
    fn setup(&self) -> ToolSetup {
        ToolSetup::new(
            self.state.clone(),
            self.executable.clone(),
            self.environment.clone(),
            Some(self.root.path().as_os_str().into()),
        )
    }
    fn installer(&self) -> Installer {
        Installer::new(
            &self.state,
            self.environment
                .resolve()
                .unwrap()
                .tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )
        .unwrap()
    }
    async fn install(&self) {
        self.installer()
            .install(
                self.environment
                    .resolve()
                    .unwrap()
                    .tool_artifacts(&[HostKind::Codex], &self.executable, &self.state)
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    fn no_execution(&self) {
        assert!(!self.root.path().join("executed").exists());
    }
}

#[tokio::test]
async fn intact_bundle_selects_exact_paths_without_execution_or_home_fallthrough() {
    let f = Fixture::new();
    f.install().await;
    let roots = f.environment.resolve().unwrap();
    let before = fs::read(&roots.codex_config).unwrap();
    let plan = f.setup().codex_launch_plan().await.unwrap();
    assert!(plan.profile().is_none());
    assert_eq!(
        plan.cli(),
        f.root.path().join("codex").canonicalize().unwrap()
    );
    assert_eq!(plan.configuration(), roots.codex_config);
    assert_eq!(plan.skill(), roots.codex_skill);
    assert_eq!(
        plan.configured_environment().unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(roots.codex_config).unwrap(), before);
    f.no_execution();
}

#[tokio::test]
async fn foreign_member_and_incomplete_bundle_are_not_adopted() {
    for incomplete in [false, true] {
        let f = Fixture::new();
        let roots = f.environment.resolve().unwrap();
        if incomplete {
            let mut artifacts = roots
                .tool_artifacts(&[HostKind::Codex], &f.executable, &f.state)
                .unwrap();
            artifacts.retain(|a| a.key != "codex.skill");
            f.installer().install(artifacts).await.unwrap();
        } else {
            fs::create_dir_all(roots.codex_config.parent().unwrap()).unwrap();
            fs::write(
                &roots.codex_config,
                "[mcp_servers.agentchat]\ncommand='/foreign/agentchat'\n",
            )
            .unwrap();
        }
        let before = fs::read(&roots.codex_config).unwrap();
        assert!(f.setup().codex_launch_plan().await.is_err());
        assert_eq!(fs::read(roots.codex_config).unwrap(), before);
        f.no_execution();
    }
}

#[tokio::test]
async fn stale_state_or_image_and_changed_member_refuse_without_repair() {
    for change in [0, 1, 2] {
        let mut f = Fixture::new();
        f.install().await;
        let roots = f.environment.resolve().unwrap();
        if change == 0 {
            f.executable = f.root.path().join("new-agentchat");
        }
        if change == 1 {
            fs::write(
                &roots.codex_config,
                "[mcp_servers.agentchat]\ncommand='/foreign/agentchat'\n",
            )
            .unwrap();
        }
        if change == 2 {
            fs::write(&roots.codex_skill, "changed skill\n").unwrap();
        }
        let before = fs::read(&roots.codex_config).unwrap();
        let result = f.setup().codex_launch_plan().await;
        assert_eq!(result.is_ok(), change == 0 && !cfg!(debug_assertions));
        assert_eq!(fs::read(roots.codex_config).unwrap(), before);
        f.no_execution();
    }
}

#[tokio::test]
async fn caller_path_policy_keeps_owned_codex_and_claude_dispatch_and_read_only_health() {
    let mut f = Fixture::new();
    let roots = f.environment.resolve().unwrap();
    let mut artifacts = roots
        .codex_managed_artifacts(&f.executable, &f.state)
        .unwrap();
    artifacts.extend(
        roots
            .claude_managed_artifacts(&f.executable, &f.state)
            .unwrap(),
    );
    f.installer().install(artifacts).await.unwrap();
    let paths = [
        &roots.codex_config,
        &roots.claude_registry,
        &roots.claude_settings,
    ];
    let before: Vec<_> = paths.iter().map(|p| fs::read(p).unwrap()).collect();
    f.executable = f.root.path().join("another-directory/release-agentchat");
    assert_eq!(
        f.setup().codex_launch_plan().await.is_ok(),
        !cfg!(debug_assertions)
    );
    let health = f.setup().inspect().await.unwrap();
    assert_eq!(
        health["tools"]["state"],
        if cfg!(debug_assertions) {
            "needs_update"
        } else {
            "healthy"
        }
    );
    for (path, original) in paths.iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), original);
    }
    f.no_execution();
}

#[tokio::test]
async fn release_inspects_each_hosts_own_registered_dispatch_path() {
    let f = Fixture::new();
    let roots = f.environment.resolve().unwrap();
    let mut artifacts = roots
        .codex_managed_artifacts(&f.root.path().join("codex-entry"), &f.state)
        .unwrap();
    artifacts.extend(
        roots
            .claude_managed_artifacts(&f.root.path().join("claude-entry"), &f.state)
            .unwrap(),
    );
    f.installer().install(artifacts).await.unwrap();
    assert_eq!(
        f.setup().codex_launch_plan().await.is_ok(),
        !cfg!(debug_assertions)
    );
    f.no_execution();
}

#[tokio::test]
async fn caller_path_policy_keeps_the_current_state_binding_even_for_owned_values() {
    let mut f = Fixture::new();
    let roots = f.environment.resolve().unwrap();
    let wrong_state = f.root.path().join("other-state");
    let artifacts = roots
        .codex_managed_artifacts(&f.executable, &wrong_state)
        .unwrap();
    f.installer().install(artifacts).await.unwrap();
    let before = fs::read(&roots.codex_config).unwrap();
    f.executable = f.root.path().join("other-agentchat");
    assert!(f.setup().codex_launch_plan().await.is_err());
    assert_eq!(fs::read(&roots.codex_config).unwrap(), before);
    assert!(!wrong_state.exists());
    f.no_execution();
}

#[tokio::test]
async fn caller_path_policy_rejects_malformed_owned_dispatch_commands() {
    for command in [
        serde_json::json!("relative-agentchat"),
        serde_json::json!(""),
        serde_json::json!(7),
    ] {
        let mut f = Fixture::new();
        let roots = f.environment.resolve().unwrap();
        let mut artifacts = roots
            .codex_managed_artifacts(&f.executable, &f.state)
            .unwrap();
        for artifact in &mut artifacts {
            if let agentchat_hosts::install::Content::Toml { value, .. } = &mut artifact.content {
                value["command"] = command.clone();
            }
        }
        f.installer().install(artifacts).await.unwrap();
        let before = fs::read(&roots.codex_config).unwrap();
        f.executable = f.root.path().join("other-agentchat");
        assert!(f.setup().codex_launch_plan().await.is_err());
        assert_eq!(fs::read(&roots.codex_config).unwrap(), before);
        f.no_execution();
    }
}

#[tokio::test]
async fn missing_cli_and_pending_installation_cannot_be_repaired_by_launch() {
    for pending in [false, true] {
        let f = Fixture::new();
        f.install().await;
        if pending {
            fs::write(f.installer().state_directory().join("pending.json"), "{}").unwrap();
        } else {
            fs::remove_file(f.root.path().join("codex")).unwrap();
        }
        assert!(f.setup().codex_launch_plan().await.is_err());
        f.no_execution();
    }
}

#[tokio::test]
async fn a_live_installer_lock_returns_busy_without_partial_selection() {
    let f = Fixture::new();
    f.install().await;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.installer().state_directory().join("install.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let error = f.setup().codex_launch_plan().await.err().unwrap();
    assert_eq!(error.code, ErrorCode::Busy);
    assert!(error.retryable);
    drop(lock);
    f.setup().codex_launch_plan().await.unwrap();
    f.no_execution();
}

#[tokio::test]
async fn unrelated_native_configuration_is_preserved() {
    let f = Fixture::new();
    f.install().await;
    let roots = f.environment.resolve().unwrap();
    let original = fs::read_to_string(&roots.codex_config).unwrap();
    fs::write(&roots.codex_config, format!("model = 'unchanged-model'\n{original}\n[mcp_servers.other]\ncommand='/foreign/server'\n")).unwrap();
    let before = fs::read(&roots.codex_config).unwrap();
    f.setup().codex_launch_plan().await.unwrap();
    assert_eq!(fs::read(roots.codex_config).unwrap(), before);
    f.no_execution();
}

#[tokio::test]
async fn explicit_native_selection_survives_public_init_and_executable_updates() {
    let mut f = Fixture::new();
    let roots = f.environment.resolve().unwrap();
    let profile = CodexProfile {
        executable: f.root.path().join("codex"),
        version: "0.163.0".into(),
    };
    let artifacts = roots
        .codex_native_artifacts(&f.executable, &f.state, &profile)
        .unwrap();
    f.installer().install(artifacts).await.unwrap();
    let original = fs::read_to_string(&roots.codex_config).unwrap();
    let foreign = "# retained foreign policy\nmodel='unchanged'\napproval_policy='on-request'\n";
    fs::write(&roots.codex_config, format!("{foreign}{original}")).unwrap();
    let before = fs::read(&roots.codex_config).unwrap();
    let setup = f.setup();
    assert_eq!(
        setup.codex_launch_plan().await.unwrap().profile(),
        Some(&profile)
    );
    let inspection = setup.inspect().await.unwrap();
    assert_eq!(inspection["tools"]["state"], "healthy");
    // Intact native configuration does not itself admit the platform. Keep
    // this preservation test valid while exact native G0 remains withheld.
    let admitted = agentchat_hosts::require_native_profile(
        HostKind::Codex,
        &profile.version,
        &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    )
    .is_ok();
    let lifecycle = if admitted { "available" } else { "unavailable" };
    assert_eq!(inspection["lifecycle"]["state"], lifecycle);
    if admitted {
        assert_eq!(
            inspection["lifecycle"]["profiles"][0]["host_version"],
            "0.163.0"
        );
    } else {
        assert!(inspection["lifecycle"]["profiles"].is_null());
    }
    let installed = setup
        .install(uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        installed["hosts"]["codex"]["components"],
        if admitted {
            serde_json::json!(["mcp", "skill", "native_gateway"])
        } else {
            serde_json::json!(["mcp", "skill"])
        }
    );
    assert_eq!(installed["lifecycle"]["state"], lifecycle);
    assert_eq!(fs::read(&roots.codex_config).unwrap(), before);
    f.executable = f.root.path().join("updated-agentchat");
    f.setup()
        .install(uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
        .await
        .unwrap();
    let after = fs::read_to_string(&roots.codex_config).unwrap();
    assert!(after.starts_with(foreign));
    assert!(after.contains("updated-agentchat"));
    assert!(after.contains("AGENTCHAT_CODEX_NATIVE_EXE"));
    assert_eq!(
        f.setup().codex_launch_plan().await.unwrap().profile(),
        Some(&profile)
    );
    assert_eq!(
        f.setup().inspect().await.unwrap()["tools"]["state"],
        "healthy"
    );
    f.no_execution();
}

#[tokio::test]
async fn foreign_or_partial_native_selectors_are_not_adopted_or_repaired() {
    for partial in [false, true] {
        let f = Fixture::new();
        let roots = f.environment.resolve().unwrap();
        let profile = CodexProfile {
            executable: f.root.path().join("codex"),
            version: "0.163.0".into(),
        };
        if partial {
            let mut artifacts = roots
                .codex_native_artifacts(&f.executable, &f.state, &profile)
                .unwrap();
            for a in &mut artifacts {
                if let agentchat_hosts::install::Content::Toml { value, .. } = &mut a.content {
                    value["env"]
                        .as_object_mut()
                        .unwrap()
                        .remove("AGENTCHAT_CODEX_NATIVE_VERSION");
                }
            }
            f.installer().install(artifacts).await.unwrap();
        } else {
            f.install().await;
            let text = fs::read_to_string(&roots.codex_config).unwrap();
            fs::write(&roots.codex_config,format!("{text}\nAGENTCHAT_CODEX_NATIVE_EXE='/foreign/codex'\nAGENTCHAT_CODEX_NATIVE_VERSION='0.163.0'\n")).unwrap();
        }
        let before = fs::read(&roots.codex_config).unwrap();
        assert!(f.setup().codex_launch_plan().await.is_err());
        assert!(
            f.setup()
                .install(uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
                .await
                .is_err()
        );
        assert_eq!(fs::read(&roots.codex_config).unwrap(), before);
        f.no_execution();
    }
}

#[test]
fn non_pinned_native_profile_planning_has_no_filesystem_side_effects() {
    let f = Fixture::new();
    let profile = CodexProfile {
        executable: f.root.path().join("codex"),
        version: "0.999.0".into(),
    };
    let artifacts = f
        .environment
        .resolve()
        .unwrap()
        .codex_native_artifacts(&f.executable, &f.state, &profile)
        .unwrap();
    assert!(artifacts.iter().any(|artifact| artifact.key == "codex.mcp"));
    assert!(!f.state.exists());
    assert!(!f.environment.user_config_root.as_ref().unwrap().exists());
    f.no_execution();
}

#[test]
fn malformed_native_profile_versions_are_rejected_without_side_effects() {
    for version in [String::new(), "bad\nversion".into(), "x".repeat(129)] {
        let f = Fixture::new();
        let profile = CodexProfile {
            executable: f.root.path().join("codex"),
            version,
        };
        assert_eq!(
            f.environment
                .resolve()
                .unwrap()
                .codex_native_artifacts(&f.executable, &f.state, &profile)
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedHost
        );
        assert!(!f.state.exists());
        assert!(!f.environment.user_config_root.as_ref().unwrap().exists());
        f.no_execution();
    }
}
