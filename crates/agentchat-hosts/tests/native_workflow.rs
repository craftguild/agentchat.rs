//! Shared public setup preserves only a verified, explicitly owned native mode.
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use agentchat_hosts::{
    claude::observer::NativeProfile,
    install::{Artifact, Content, InstallProgress, InstallStage, Installer},
    setup::{Environment, Roots, ToolSetup},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};
use uuid::Uuid;
struct Fixture {
    base: tempfile::TempDir,
    environment: Environment,
    roots: Roots,
    state: PathBuf,
    agentchat: PathBuf,
    bin: PathBuf,
    profile: NativeProfile,
    installer: Installer,
}
impl Fixture {
    fn new() -> Self {
        let base = tempfile::tempdir().unwrap();
        let config = base.path().join("config");
        fs::create_dir(&config).unwrap();
        let environment = Environment {
            user_config_root: Some(config),
            ..Environment::default()
        };
        let roots = environment.resolve().unwrap();
        let state = base.path().join("state");
        let agentchat = base.path().join("agentchat");
        let bin = base.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let native = bin.join("claude");
        let called = base.path().join("host-executed");
        let marker = called.to_str().unwrap().replace('\'', "'\"'\"'");
        fs::write(
            &native,
            format!("#!/bin/sh\nprintf called > '{marker}'\nexit 91\n"),
        )
        .unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
        let profile = NativeProfile {
            executable: native,
            version: "2.1.301".into(),
        };
        let installer = Installer::new(
            &state,
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )
        .unwrap();
        Self {
            base,
            environment,
            roots,
            state,
            agentchat,
            bin,
            profile,
            installer,
        }
    }
    fn setup(&self) -> ToolSetup {
        self.setup_at(self.agentchat.clone())
    }
    fn setup_at(&self, agentchat: PathBuf) -> ToolSetup {
        ToolSetup::new(
            self.state.clone(),
            agentchat,
            self.environment.clone(),
            Some(self.bin.clone().into_os_string()),
        )
    }
    fn plan(&self) -> Vec<Artifact> {
        self.roots
            .claude_native_artifacts(&self.agentchat, &self.state, &self.profile)
            .unwrap()
    }
    async fn native(&self) {
        self.installer.install(self.plan()).await.unwrap();
    }
    fn registry(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.roots.claude_registry).unwrap()).unwrap()
    }
    fn module(&self) -> String {
        fs::read_to_string(
            self.roots
                .claude_skill
                .parent()
                .unwrap()
                .join("hooks/register.js"),
        )
        .unwrap()
    }
    fn assert_no_execution(&self) {
        assert!(!self.base.path().join("host-executed").exists());
    }
    async fn healthy(&self) {
        let status = self.setup().inspect().await.unwrap();
        assert_eq!(status["tools"]["state"], "healthy", "{status}");
        assert_eq!(status["tools"]["components"].as_array().unwrap().len(), 9);
        assert_eq!(status["lifecycle"]["state"], "available");
        assert_eq!(
            status["lifecycle"]["profiles"][0]["compatibility"],
            "best_effort_api_surface"
        );
        self.assert_no_execution();
    }
}
#[tokio::test]
async fn a_new_public_init_and_current_health_preserve_the_owned_native_plan() {
    let f = Fixture::new();
    f.native().await;
    f.healthy().await;
    let before = f.registry();
    let module = f.module();
    f.setup()
        .install(Uuid::new_v4(), Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        f.registry(),
        before,
        "public init must not downgrade native MCP configuration"
    );
    assert_eq!(f.module(), module);
    f.healthy().await;
}
struct Fail;
impl InstallProgress for Fail {
    fn reached(&self, stage: InstallStage) -> Result<()> {
        if stage == InstallStage::Published(0) {
            Err(Error::new(ErrorCode::IoError, "Injected native cut."))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn public_init_recovers_native_intent_before_selecting_its_next_plan() {
    let f = Fixture::new();
    f.setup()
        .install(Uuid::new_v4(), Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        f.installer
            .clone()
            .with_progress(Arc::new(Fail))
            .install(f.plan())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IoError
    );
    assert_eq!(
        f.setup().inspect().await.unwrap()["tools"]["state"],
        "recovery_required"
    );
    f.setup()
        .install(Uuid::new_v4(), Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        f.registry()["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_EXE"],
        f.profile.executable.to_str().unwrap()
    );
    f.healthy().await;
}
#[tokio::test]
async fn old_completed_retries_do_not_rewind_native_selection_and_new_requests_update_all_owned_paths()
 {
    let f = Fixture::new();
    let request = Uuid::new_v4();
    let project = Uuid::new_v4();
    let old = f.setup().install(request, project).await.unwrap();
    f.native().await;
    let before = f.registry();
    let module = f.module();
    assert_eq!(f.setup().install(request, project).await.unwrap(), old);
    assert_eq!(f.registry(), before);
    assert_eq!(f.module(), module);
    let agentchat = f.base.path().join("updated agentchat 'literal'");
    let setup = f.setup_at(agentchat.clone());
    setup.install(Uuid::new_v4(), project).await.unwrap();
    let after = f.registry();
    assert_eq!(
        after["mcpServers"]["agentchat"]["command"],
        agentchat.to_str().unwrap()
    );
    assert_eq!(
        after["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_EXE"],
        before["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_EXE"]
    );
    assert!(
        f.module()
            .contains(&serde_json::to_string(agentchat.to_str().unwrap()).unwrap())
    );
    let stable = fs::read(&f.roots.claude_registry).unwrap();
    setup.install(Uuid::new_v4(), project).await.unwrap();
    assert_eq!(fs::read(&f.roots.claude_registry).unwrap(), stable);
    assert_eq!(setup.inspect().await.unwrap()["tools"]["state"], "healthy");
    f.assert_no_execution();
}
#[tokio::test]
async fn edited_native_selection_refuses_without_adopting_or_rewriting_foreign_values() {
    let f = Fixture::new();
    f.native().await;
    let mut value = f.registry();
    value["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_EXE"] =
        json!("/foreign/native");
    fs::write(
        &f.roots.claude_registry,
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let before = fs::read(&f.roots.claude_registry).unwrap();
    let ledger = fs::read(f.installer.state_directory().join("ownership.json")).unwrap();
    assert_eq!(
        f.setup()
            .install(Uuid::new_v4(), Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(
        f.setup().inspect().await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(&f.roots.claude_registry).unwrap(), before);
    assert_eq!(
        fs::read(f.installer.state_directory().join("ownership.json")).unwrap(),
        ledger
    );
    f.assert_no_execution();
}
async fn invalid_owned_version(value: Option<&str>, code: ErrorCode) {
    let f = Fixture::new();
    f.native().await;
    let mut plan = f.plan();
    for artifact in &mut plan {
        if artifact.key == "claude.mcp"
            && let Content::Json { value: mcp, .. } = &mut artifact.content
        {
            let env = mcp["env"].as_object_mut().unwrap();
            match value {
                Some(value) => {
                    env.insert("AGENTCHAT_CLAUDE_NATIVE_VERSION".into(), json!(value));
                }
                None => {
                    env.remove("AGENTCHAT_CLAUDE_NATIVE_VERSION");
                }
            }
        }
    }
    f.installer.install(plan).await.unwrap();
    let before = fs::read(&f.roots.claude_registry).unwrap();
    assert_eq!(
        f.setup()
            .install(Uuid::new_v4(), Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        code
    );
    assert_eq!(f.setup().inspect().await.unwrap_err().code, code);
    assert_eq!(fs::read(&f.roots.claude_registry).unwrap(), before);
    f.assert_no_execution();
}
#[tokio::test]
async fn missing_owned_native_version_requires_explicit_supported_update() {
    invalid_owned_version(None, ErrorCode::ConfigurationConflict).await;
}
#[tokio::test]
async fn control_bearing_owned_native_version_never_falls_back_to_generic_setup() {
    invalid_owned_version(Some("future\nversion"), ErrorCode::UnsupportedHost).await;
}
#[tokio::test]
async fn an_unowned_native_mcp_value_never_selects_or_installs_a_native_bundle() {
    let f = Fixture::new();
    fs::create_dir_all(f.roots.claude_registry.parent().unwrap()).unwrap();
    let value = json!({"mcpServers":{"agentchat":{"command":"foreign","env":{"AGENTCHAT_CLAUDE_NATIVE_EXE":f.profile.executable,"AGENTCHAT_CLAUDE_NATIVE_VERSION":"2.1.301"}}}});
    fs::write(
        &f.roots.claude_registry,
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert_eq!(
        f.setup().inspect().await.unwrap()["tools"]["state"],
        "not_installed"
    );
    assert_eq!(
        f.setup()
            .install(Uuid::new_v4(), Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(f.registry(), value);
    assert!(!f.roots.claude_skill.exists());
    f.assert_no_execution();
}
