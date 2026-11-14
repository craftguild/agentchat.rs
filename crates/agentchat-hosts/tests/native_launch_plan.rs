//! Owned launch selection is inspection, never host execution or repair.
use agentchat_core::{ErrorCode, model::HostKind};
use agentchat_hosts::{
    claude::observer::NativeProfile,
    install::{Artifact, Installer},
    setup::{Environment, Roots, ToolSetup},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    root: tempfile::TempDir,
    roots: Roots,
    environment: Environment,
    agentchat: PathBuf,
    state: PathBuf,
    profile: NativeProfile,
    installer: Installer,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let environment = Environment {
            user_config_root: Some(config.clone()),
            claude_config_dir: Some(config.join("native")),
            ..Environment::default()
        };
        let roots = environment.resolve().unwrap();
        let agentchat = root.path().join("agentchat");
        let state = root.path().join("state");
        let native = root.path().join("claude");
        fs::write(
            &native,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 91\n",
                root.path().join("executed").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
        let installer = Installer::new(
            &state,
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )
        .unwrap();
        Self {
            root,
            roots,
            environment,
            agentchat,
            state,
            profile: NativeProfile {
                executable: native,
                version: "2.1.302".into(),
            },
            installer,
        }
    }
    fn setup(&self) -> ToolSetup {
        ToolSetup::new(
            self.state.clone(),
            self.agentchat.clone(),
            self.environment.clone(),
            None,
        )
    }
    fn artifacts(&self) -> Vec<Artifact> {
        self.roots
            .claude_native_artifacts(&self.agentchat, &self.state, &self.profile)
            .unwrap()
    }
    fn module(&self) -> PathBuf {
        self.roots
            .claude_skill
            .parent()
            .unwrap()
            .join("hooks/register.js")
    }
    fn no_execution(&self) {
        assert!(!self.root.path().join("executed").exists());
    }
}

#[tokio::test]
async fn intact_owned_plan_retains_paths_and_foreign_settings_without_executing_host() {
    let f = Fixture::new();
    f.installer.install(f.artifacts()).await.unwrap();
    let mut settings: Value =
        serde_json::from_slice(&fs::read(&f.roots.claude_settings).unwrap()).unwrap();
    settings["permissions"] = json!({"defaultMode":"plan"});
    fs::write(
        &f.roots.claude_settings,
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    let before = fs::read(&f.roots.claude_settings).unwrap();
    let module = fs::read(f.module()).unwrap();
    let plan = f.setup().claude_launch_plan().await.unwrap();
    assert_eq!(plan.profile().executable, f.profile.executable);
    assert_eq!(plan.profile().version, "2.1.302");
    assert_eq!(
        plan.plugin_directory(),
        f.roots.claude_skill.parent().unwrap()
    );
    assert_eq!(plan.settings(), f.roots.claude_settings);
    assert_eq!(plan.registry(), f.roots.claude_registry);
    assert_eq!(fs::read(&f.roots.claude_settings).unwrap(), before);
    assert_eq!(fs::read(f.module()).unwrap(), module);
    f.no_execution();
}

#[tokio::test]
async fn missing_or_foreign_native_selector_cannot_create_or_adopt_an_installation() {
    let f = Fixture::new();
    assert!(f.setup().claude_launch_plan().await.is_err());
    assert!(!f.state.exists());
    f.installer
        .install(
            f.roots
                .tool_artifacts(&[HostKind::Claude], &f.agentchat, &f.state)
                .unwrap(),
        )
        .await
        .unwrap();
    let mut registry: Value =
        serde_json::from_slice(&fs::read(&f.roots.claude_registry).unwrap()).unwrap();
    registry["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_EXE"] =
        json!(f.profile.executable);
    registry["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_VERSION"] =
        json!("2.1.302");
    let before = serde_json::to_vec(&registry).unwrap();
    fs::write(&f.roots.claude_registry, &before).unwrap();
    assert!(f.setup().claude_launch_plan().await.is_err());
    assert_eq!(fs::read(&f.roots.claude_registry).unwrap(), before);
    assert!(!f.module().exists());
    f.no_execution();
}

#[tokio::test]
async fn changed_module_or_native_selector_refuses_without_repair() {
    for selector in [false, true] {
        let f = Fixture::new();
        f.installer.install(f.artifacts()).await.unwrap();
        let path = if selector {
            &f.roots.claude_registry
        } else {
            &f.module()
        };
        let bytes = if selector {
            let mut v: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            v["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_EXE"] =
                json!("/foreign/claude");
            serde_json::to_vec(&v).unwrap()
        } else {
            b"// changed outside installer\n".to_vec()
        };
        fs::write(path, &bytes).unwrap();
        assert!(f.setup().claude_launch_plan().await.is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
        f.no_execution();
    }
}

#[tokio::test]
async fn incomplete_302_bundle_and_pending_installation_cannot_supply_a_plan() {
    for missing in ["claude.native.input", "claude.skill", "pending"] {
        let f = Fixture::new();
        let mut artifacts = f.artifacts();
        if missing != "pending" {
            artifacts.retain(|a| a.key != missing);
        }
        f.installer.install(artifacts).await.unwrap();
        if missing == "pending" {
            fs::write(f.installer.state_directory().join("pending.json"), b"{}").unwrap();
        }
        let ledger = fs::read(f.installer.state_directory().join("ownership.json")).unwrap();
        assert!(f.setup().claude_launch_plan().await.is_err());
        assert_eq!(
            fs::read(f.installer.state_directory().join("ownership.json")).unwrap(),
            ledger
        );
        f.no_execution();
    }
}

#[tokio::test]
async fn a_new_agentchat_executable_requires_update_and_a_future_label_uses_the_current_bundle() {
    let f = Fixture::new();
    f.installer.install(f.artifacts()).await.unwrap();
    let setup = ToolSetup::new(
        f.state.clone(),
        f.root.path().join("new-agentchat"),
        f.environment.clone(),
        None,
    );
    assert_eq!(
        setup.claude_launch_plan().await.is_ok(),
        !cfg!(debug_assertions)
    );
    // A separately owned future label uses the same current full-input bundle.
    let mut g = Fixture::new();
    g.profile.version = "future-canary".into();
    g.installer.install(g.artifacts()).await.unwrap();
    let result = g.setup().claude_launch_plan().await;
    assert_eq!(result.unwrap().profile().version, "future-canary");
    f.no_execution();
    g.no_execution();
}

#[tokio::test]
async fn caller_path_policy_never_adopts_modified_owned_claude_configuration() {
    for change in 0..4 {
        let f = Fixture::new();
        f.installer.install(f.artifacts()).await.unwrap();
        let module = f.module();
        let path = match change {
            0 | 1 => &f.roots.claude_registry,
            2 => &f.roots.claude_settings,
            _ => &module,
        };
        let mut bytes = fs::read(path).unwrap();
        if change < 2 {
            let mut value: Value = serde_json::from_slice(&bytes).unwrap();
            if change == 0 {
                value["mcpServers"]["agentchat"]["command"] = json!("/foreign/agentchat");
            } else {
                value["mcpServers"]["agentchat"]["env"]["AGENTCHAT_HOME"] = json!("/foreign/state");
            }
            bytes = serde_json::to_vec(&value).unwrap();
        } else if change == 2 {
            let mut value: Value = serde_json::from_slice(&bytes).unwrap();
            value["hooks"]["FileChanged"][0]["hooks"][0]["command"] = json!("/foreign/hook");
            bytes = serde_json::to_vec(&value).unwrap();
        } else {
            bytes.extend_from_slice(b"\n// changed outside installer\n");
        }
        fs::write(path, &bytes).unwrap();
        let setup = ToolSetup::new(
            f.state.clone(),
            f.root.path().join("other-agentchat"),
            f.environment.clone(),
            None,
        );
        assert!(setup.claude_launch_plan().await.is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
        f.no_execution();
    }
}

#[tokio::test]
async fn a_live_installer_lock_returns_retryable_busy_without_partial_selection() {
    let f = Fixture::new();
    f.installer.install(f.artifacts()).await.unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.installer.state_directory().join("install.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let result = f.setup().claude_launch_plan().await;
    assert!(matches!(result, Err(e) if e.code == ErrorCode::Busy && e.retryable));
    drop(lock);
    assert!(f.setup().claude_launch_plan().await.is_ok());
    f.no_execution();
}

#[tokio::test]
async fn configured_environment_selects_the_owned_native_directory_without_host_home_overrides() {
    let f = Fixture::new();
    f.installer.install(f.artifacts()).await.unwrap();
    let plan = f.setup().claude_launch_plan().await.unwrap();
    let env = plan.configured_environment().unwrap();
    assert_eq!(
        env[std::ffi::OsStr::new("AGENTCHAT_HOME")],
        f.state.as_os_str()
    );
    assert_eq!(
        env[std::ffi::OsStr::new("CLAUDE_CONFIG_DIR")],
        f.environment
            .claude_config_dir
            .as_ref()
            .unwrap()
            .as_os_str()
    );
    assert_eq!(env.len(), 3);
    assert!(!env.contains_key(std::ffi::OsStr::new("HOME")));
    assert!(!env.contains_key(std::ffi::OsStr::new("CODEX_HOME")));
    f.no_execution();
}

#[tokio::test]
async fn an_isolated_tool_layout_cannot_fall_back_to_the_real_native_home() {
    let mut f = Fixture::new();
    f.environment.claude_config_dir = None;
    f.roots = f.environment.resolve().unwrap();
    f.installer = Installer::new(
        &f.state,
        f.roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
    )
    .unwrap();
    f.installer.install(f.artifacts()).await.unwrap();
    let plan = f.setup().claude_launch_plan().await.unwrap();
    assert!(
        matches!(plan.configured_environment(), Err(e) if e.code == ErrorCode::ConfigurationConflict)
    );
    f.no_execution();
}
