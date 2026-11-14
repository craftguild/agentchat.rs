//! Captured PATH selection, fixed request reports and preserving host upgrades.
use agentchat_core::ErrorCode;
use agentchat_hosts::setup::{Environment, ToolSetup};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};
use uuid::Uuid;

struct Rig {
    root: tempfile::TempDir,
    state: PathBuf,
    config: PathBuf,
    bin: PathBuf,
}
impl Rig {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let config = root.path().join("config with spaces");
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        Self {
            root,
            state,
            config,
            bin,
        }
    }
    fn host(&self, name: &str) {
        let path = self.bin.join(name);
        // Presence is not host execution or admission. Executing this fixture
        // would produce an observable unwanted file and fail the assertions.
        fs::write(
            &path,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 77\n",
                self.root.path().join("HOST_EXECUTED").display()
            ),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn setup(&self, binary: &str) -> ToolSetup {
        ToolSetup::new(
            self.state.clone(),
            self.root.path().join(binary),
            Environment {
                user_config_root: Some(self.config.clone()),
                ..Environment::default()
            },
            Some(self.bin.clone().into_os_string()),
        )
    }
    fn claude(&self) -> Value {
        serde_json::from_slice(&fs::read(self.config.join(".claude.json")).unwrap()).unwrap()
    }
}

#[tokio::test]
async fn host_addition_preserves_policy_and_old_retries_cannot_rewind_binary_updates() {
    let r = Rig::new();
    r.host("codex");
    fs::create_dir_all(r.config.join(".codex")).unwrap();
    let policy =
        "# retained\napproval_policy = 'on-request'\n[mcp_servers.foreign]\ncommand = 'foreign'\n";
    fs::write(r.config.join(".codex/config.toml"), policy).unwrap();
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let old = r.setup("agentchat-old");
    let first = old.install(id, project).await.unwrap();
    assert_eq!(first["state"], "partial");
    assert_eq!(first["hosts"]["codex"]["state"], "tools_installed");
    assert_eq!(first["hosts"]["claude"]["state"], "not_installed");
    assert!(!r.config.join(".claude.json").exists());
    r.host("claude");
    assert_eq!(old.install(id, project).await.unwrap(), first);
    assert!(
        !r.config.join(".claude.json").exists(),
        "successful retry cannot add new artifacts"
    );
    let fresh = r.setup("agentchat-new");
    let next = fresh.install(Uuid::new_v4(), project).await.unwrap();
    assert_eq!(
        next["hosts"]["claude"]["components"],
        json!(["mcp", "skill"])
    );
    assert_eq!(
        r.claude()["mcpServers"]["agentchat"]["command"],
        r.root.path().join("agentchat-new").to_str().unwrap()
    );
    let codex = fs::read(r.config.join(".codex/config.toml")).unwrap();
    assert!(std::str::from_utf8(&codex).unwrap().starts_with(policy));
    assert_eq!(old.install(id, project).await.unwrap(), first);
    assert_eq!(
        fs::read(r.config.join(".codex/config.toml")).unwrap(),
        codex
    );
    assert!(!r.root.path().join("HOST_EXECUTED").exists());
    assert!(!r.config.join(".codex/hooks.json").exists());
    assert!(!r.config.join(".claude/settings.json").exists());
}

#[tokio::test]
async fn missing_nonexecutable_directory_and_dangling_entries_do_not_create_host_files() {
    let r = Rig::new();
    fs::write(r.bin.join("codex"), "not executable").unwrap();
    fs::set_permissions(r.bin.join("codex"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir(r.bin.join("claude")).unwrap();
    let setup = r.setup("agentchat");
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let first = setup.install(id, project).await.unwrap();
    assert_eq!(first["state"], "not_installed");
    assert_eq!(first["lifecycle"]["state"], "unavailable");
    assert!(!r.config.exists());
    fs::remove_file(r.bin.join("codex")).unwrap();
    symlink("absent", r.bin.join("codex")).unwrap();
    assert_eq!(setup.install(Uuid::new_v4(), project).await.unwrap(), first);
    assert!(!r.config.exists());
}

#[tokio::test]
async fn concurrent_requests_share_one_owned_installation_and_fence_other_namespaces() {
    let r = Rig::new();
    r.host("codex");
    r.host("claude");
    let setup = r.setup("agentchat");
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let (a, b, c) = tokio::join!(
        setup.install(id, project),
        setup.install(id, project),
        setup.install(Uuid::new_v4(), project)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    c.unwrap();
    let before = r.claude();
    assert_eq!(
        setup.install(id, Uuid::new_v4()).await.unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(r.claude(), before);
    assert!(!r.state.join("integrations/pending.json").exists());
}

#[tokio::test]
async fn completed_full_response_precedes_discovery_and_ignores_later_foreign_edits() {
    let r = Rig::new();
    r.host("claude");
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let first = r.setup("agentchat-old").install(id, project).await.unwrap();
    let path = r
        .state
        .join("integrations/requests")
        .join(format!("{id}.json"));
    let receipt = fs::read(&path).unwrap();
    let recorded: Value = serde_json::from_slice(&receipt).unwrap();
    assert_eq!(recorded["version"], 2);
    assert_eq!(recorded["report"], first);
    r.host("codex");
    let mut foreign = r.claude();
    foreign["mcpServers"]["agentchat"]["command"] = json!("/foreign/edited");
    let registry = serde_json::to_vec(&foreign).unwrap();
    fs::write(r.config.join(".claude.json"), &registry).unwrap();
    let ledger = fs::read(r.state.join("integrations/ownership.json")).unwrap();
    let replay = r
        .setup("agentchat-new")
        .with_native_discovery()
        .install(id, project)
        .await
        .unwrap();
    assert_eq!(replay, first);
    assert_eq!(fs::read(&path).unwrap(), receipt);
    assert_eq!(fs::read(r.config.join(".claude.json")).unwrap(), registry);
    assert_eq!(
        fs::read(r.state.join("integrations/ownership.json")).unwrap(),
        ledger
    );
    assert!(!r.config.join(".codex/config.toml").exists());
    assert!(!r.root.path().join("HOST_EXECUTED").exists());
}

#[tokio::test]
async fn legacy_host_only_success_refuses_without_discovery_or_rewriting_and_new_init_can_proceed()
{
    let r = Rig::new();
    r.host("claude");
    let setup = r.setup("agentchat");
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    setup.install(id, project).await.unwrap();
    let path = r
        .state
        .join("integrations/requests")
        .join(format!("{id}.json"));
    let legacy = serde_json::to_vec(
        &json!({"version":1,"request":id,"namespace":project,"hosts":["claude"]}),
    )
    .unwrap();
    fs::write(&path, &legacy).unwrap();
    let registry = fs::read(r.config.join(".claude.json")).unwrap();
    let ledger = fs::read(r.state.join("integrations/ownership.json")).unwrap();
    let error = setup
        .clone()
        .with_native_discovery()
        .install(id, project)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigurationConflict);
    assert_eq!(
        error.details["reason"],
        "historical_installation_result_unavailable"
    );
    assert!(!error.retryable);
    assert!(error.message.contains("status") && error.message.contains("new request ID"));
    assert_eq!(fs::read(&path).unwrap(), legacy);
    assert_eq!(fs::read(r.config.join(".claude.json")).unwrap(), registry);
    assert_eq!(
        fs::read(r.state.join("integrations/ownership.json")).unwrap(),
        ledger
    );
    assert!(!r.root.path().join("HOST_EXECUTED").exists());
    assert_eq!(
        setup.install(id, Uuid::new_v4()).await.unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        setup.install(Uuid::new_v4(), project).await.unwrap()["hosts"]["claude"]["state"],
        "tools_installed"
    );
    assert_eq!(fs::read(&path).unwrap(), legacy);
}

#[tokio::test]
async fn invalid_or_symbolic_request_receipts_are_refused_without_host_rewrites() {
    let r = Rig::new();
    r.host("claude");
    let setup = r.setup("agentchat");
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    setup.install(id, project).await.unwrap();
    let path = r
        .state
        .join("integrations/requests")
        .join(format!("{id}.json"));
    let original = fs::read(&path).unwrap();
    let before = fs::read(r.config.join(".claude.json")).unwrap();
    let mut value: Value;
    for invalid in [
        json!({"version":999}),
        json!({"hosts":["claude","claude"]}),
        json!({"hosts":["unknown-host"]}),
        json!({"hosts":[]}),
        json!({"report":null}),
        json!({"report":{"state":"partial","hosts":{},"lifecycle":{}}}),
        json!({"unexpected":true}),
    ] {
        value = serde_json::from_slice(&original).unwrap();
        for (key, field) in invalid.as_object().unwrap() {
            value[key] = field.clone();
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            setup.install(id, project).await.unwrap_err().code,
            ErrorCode::ConfigurationConflict
        );
    }
    fs::remove_file(&path).unwrap();
    let foreign = r.root.path().join("foreign");
    fs::write(&foreign, &original).unwrap();
    symlink(&foreign, &path).unwrap();
    assert_eq!(
        setup.install(id, project).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(r.config.join(".claude.json")).unwrap(), before);
    assert_eq!(fs::read(foreign).unwrap(), original);
}
