//! Explicit candidate installation component tests; no admission or host starts.
use super::*;
use crate::setup::{CodexProfile, Environment};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};

struct Fixture {
    root: tempfile::TempDir,
    setup: ToolSetup,
    profile: NativeProfile,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let native = config.join("host-config");
        fs::create_dir_all(&native).unwrap();
        fs::write(native.join("settings.json"),json!({"permissions":{"defaultMode":"plan"},"hooks":{"FileChanged":[{"matcher":"foreign","hooks":[{"type":"command","command":"foreign"}]}]}}).to_string()).unwrap();
        fs::write(native.join(".claude.json"),json!({"mcpServers":{"foreign":{"command":"foreign"}},"projects":{"/foreign":{"hasTrustDialogAccepted":false}}}).to_string()).unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let launcher = bin.join("claude");
        fs::write(
            &launcher,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 91\n",
                root.path().join("executed").display()
            ),
        )
        .unwrap();
        fs::set_permissions(launcher, fs::Permissions::from_mode(0o700)).unwrap();
        let setup = ToolSetup::new(
            root.path().join("state"),
            root.path().join("agentchat"),
            Environment {
                user_config_root: Some(config),
                claude_config_dir: Some(native),
                ..Default::default()
            },
            Some(bin.into_os_string()),
        );
        let profile = NativeProfile {
            executable: PathBuf::from("/bin/sh").canonicalize().unwrap(),
            version: "2.1.302".into(),
        };
        Self {
            root,
            setup,
            profile,
        }
    }
    fn roots(&self) -> super::super::Roots {
        self.setup.environment.resolve().unwrap()
    }
    fn module(&self) -> PathBuf {
        self.roots()
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
async fn first_native_selection_preserves_foreign_policy_and_is_a_current_owned_plan() {
    let f = Fixture::new();
    let key = Uuid::new_v4();
    let project = Uuid::new_v4();
    f.setup
        .install_selected(key, project, None, Some(f.profile.clone()))
        .await
        .unwrap();
    assert!(f.module().is_file());
    assert!(f.module().with_file_name("input.js").is_file());
    // Installation no longer fixes the observed engine. A future launch must
    // resolve PATH, and this fixture's shell launcher is deliberately refused
    // before execution rather than falling back to the old /bin/sh profile.
    assert_eq!(
        f.setup.claude_launch_plan().await.err().unwrap().code,
        ErrorCode::UnsupportedHost
    );
    let roots = f.roots();
    let settings: Value =
        serde_json::from_slice(&fs::read(&roots.claude_settings).unwrap()).unwrap();
    assert_eq!(settings["permissions"]["defaultMode"], "plan");
    assert_eq!(settings["hooks"]["FileChanged"][0]["matcher"], "foreign");
    let registry: Value =
        serde_json::from_slice(&fs::read(&roots.claude_registry).unwrap()).unwrap();
    assert_eq!(registry["mcpServers"]["foreign"]["command"], "foreign");
    assert_eq!(
        registry["projects"]["/foreign"]["hasTrustDialogAccepted"],
        false
    );
    let module = fs::read(f.module()).unwrap();
    let mut changed = f.profile.clone();
    changed.version = "2.1.301".into();
    f.setup
        .install_selected(key, project, None, Some(changed.clone()))
        .await
        .unwrap();
    assert_eq!(fs::read(f.module()).unwrap(), module);
    f.setup
        .install_selected(Uuid::new_v4(), project, None, Some(changed))
        .await
        .unwrap();
    assert_eq!(fs::read(f.module()).unwrap(), module);
    f.no_execution();
}

#[tokio::test]
async fn explicit_update_migrates_owned_fixed_hosts_without_reopening_removed_engines() {
    let f = Fixture::new();
    let roots = f.roots();
    let removed = f.root.path().join("removed-engine");
    let old_claude = NativeProfile {
        executable: removed.clone(),
        version: "2.1.309".into(),
    };
    let old_codex = CodexProfile {
        executable: removed.clone(),
        version: "0.165.0".into(),
    };
    let codex = f.root.path().join("bin/codex");
    fs::copy(f.root.path().join("bin/claude"), &codex).unwrap();
    let installer = Installer::new(
        &f.setup.state,
        roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
    )
    .unwrap();
    let mut old = roots
        .claude_native_artifacts(&f.setup.executable, &f.setup.state, &old_claude)
        .unwrap();
    old.extend(
        roots
            .codex_native_artifacts(&f.setup.executable, &f.setup.state, &old_codex)
            .unwrap(),
    );
    installer.install(old).await.unwrap();
    assert!(!removed.exists());
    f.setup
        .install_selected(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Some(old_codex),
            Some(f.profile.clone()),
        )
        .await
        .unwrap();
    let registry: Value =
        serde_json::from_slice(&fs::read(&roots.claude_registry).unwrap()).unwrap();
    let env = &registry["mcpServers"]["agentchat"]["env"];
    assert_eq!(env["AGENTCHAT_CLAUDE_MANAGED"], "1");
    assert!(env.get("AGENTCHAT_CLAUDE_NATIVE_EXE").is_none());
    assert_eq!(registry["mcpServers"]["foreign"]["command"], "foreign");
    assert_eq!(
        registry["projects"]["/foreign"]["hasTrustDialogAccepted"],
        false
    );
    let settings: Value =
        serde_json::from_slice(&fs::read(&roots.claude_settings).unwrap()).unwrap();
    assert_eq!(settings["permissions"]["defaultMode"], "plan");
    let module = fs::read_to_string(f.module()).unwrap();
    assert!(module.contains("@managed"));
    assert!(!module.contains(removed.to_str().unwrap()));
    let codex = fs::read_to_string(&roots.codex_config).unwrap();
    assert!(codex.contains("AGENTCHAT_CODEX_MANAGED"));
    assert!(!codex.contains("AGENTCHAT_CODEX_NATIVE_EXE"));
    assert!(!codex.contains(removed.to_str().unwrap()));
    f.no_execution();
}

#[tokio::test]
async fn foreign_selector_edit_is_not_replaced_by_new_discovery() {
    let f = Fixture::new();
    let project = Uuid::new_v4();
    f.setup
        .install_selected(Uuid::new_v4(), project, None, Some(f.profile.clone()))
        .await
        .unwrap();
    let roots = f.roots();
    let original_module = fs::read(f.module()).unwrap();
    let mut registry: Value =
        serde_json::from_slice(&fs::read(&roots.claude_registry).unwrap()).unwrap();
    registry["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_VERSION"] =
        json!("foreign");
    fs::write(&roots.claude_registry, registry.to_string()).unwrap();
    let before = fs::read(&roots.claude_registry).unwrap();
    assert!(
        f.setup
            .install_selected(Uuid::new_v4(), project, None, Some(f.profile.clone()))
            .await
            .is_err()
    );
    assert_eq!(fs::read(&roots.claude_registry).unwrap(), before);
    assert_eq!(fs::read(f.module()).unwrap(), original_module);
    f.no_execution();
}

#[tokio::test]
async fn public_discovery_never_executes_an_unadmitted_or_script_host() {
    let f = Fixture::new();
    f.setup
        .clone()
        .with_native_discovery()
        .install(Uuid::new_v4(), Uuid::new_v4())
        .await
        .unwrap();
    assert!(!f.module().exists());
    f.no_execution();
}

#[test]
fn lifecycle_reports_both_independently_qualified_hosts() {
    let f = Fixture::new();
    let codex = CodexProfile {
        executable: "/test/codex".into(),
        version: "0.163.0".into(),
    };
    use agentchat_protocol::integration::LifecycleHealth;
    let LifecycleHealth::Available { profiles } = lifecycle(Some(&codex), Some(&f.profile)) else {
        panic!("intact profiles must be reported")
    };
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].host, HostKind::Codex);
    assert_eq!(profiles[1].host, HostKind::Claude);
    assert!(matches!(
        lifecycle(None, None),
        LifecycleHealth::Unavailable { .. }
    ));
}

#[test]
fn optional_discovery_preserves_named_api_surface_errors() {
    let missing = crate::missing_api(HostKind::Codex, Some("future"), &["thread/read"]);
    let error = optional_candidate::<()>(Err(missing)).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["missing_api"], json!(["thread/read"]));
    assert!(
        optional_candidate::<()>(Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "No running candidate.",
        )))
        .unwrap()
        .is_none()
    );
}
