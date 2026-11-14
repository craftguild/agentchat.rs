//! CLI/MCP diagnostics inspect current config without repair or participation.
mod support;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::{
    install::{InstallProgress, InstallStage},
    setup::{Environment, ToolSetup},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path, process::Stdio, sync::Arc,
    time::Duration,
};
use support::{Fixture, mcp::Mcp};
use uuid::Uuid;

async fn command(f: &Fixture, args: &[&str], path: &Path) -> Value {
    let child = f.command(args).env("PATH", path).spawn().unwrap();
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
async fn peer(f: &Fixture, path: &Path) -> Mcp {
    Mcp::from_child(
        f.command(&["mcp"])
            .env("PATH", path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    )
    .await
}
fn tree(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        files.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            if path.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            },
        );
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), files);
            }
        }
    }
    let mut files = BTreeMap::new();
    if root.exists() {
        visit(root, root, &mut files);
    }
    files
}
fn path(f: &Fixture, codex: bool) -> std::path::PathBuf {
    let bin = f.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    if codex {
        let p = bin.join("codex");
        fs::write(&p, "#!/bin/sh\nexit 77\n").unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}
async fn check(f: &Fixture, mcp: &mut Mcp, bin: &Path, operation: &str, state: &str) {
    let args = if operation == "whoami" {
        vec!["--json", "--by-user", operation]
    } else {
        vec!["--json", operation]
    };
    let cli = command(f, &args, bin).await;
    // The unbound MCP diagnostic stays actor-free. User Whoami carries the same
    // current integration health without creating participation or a descriptor.
    let api = mcp
        .call("agentchat_doctor", json!({"project_path":f.workspace}))
        .await;
    assert_eq!(cli["result"]["integrations"], api["result"]["integrations"]);
    assert_eq!(cli["result"]["integrations"]["tools"]["state"], state);
    assert_eq!(
        cli["result"]["integrations"]["lifecycle"]["state"],
        "unavailable"
    );
    assert_eq!(
        cli["result"]["capabilities"],
        agentchat_hosts::capabilities()
    );
    if operation == "doctor" {
        assert_eq!(cli["result"]["database"]["integrity"], "ok");
    } else {
        assert_eq!(cli["result"]["participation"], "unjoined");
        assert!(cli["result"]["actor"].is_null());
        assert!(cli["result"]["context_id"].is_null());
        let unbound = mcp
            .call("agentchat_whoami", json!({"project_path":f.workspace}))
            .await;
        assert_eq!(unbound["error"]["code"], "session_required");
    }
}

#[tokio::test]
async fn offline_doctor_reports_no_owned_setup_without_creating_config_or_lock() {
    let f = Fixture::new();
    let project = f.initialize().await;
    let bin = path(&f, false);
    let before = tree(&f.config);
    let mut mcp = peer(&f, &bin).await;
    check(&f, &mut mcp, &bin, "doctor", "not_installed").await;
    assert_eq!(tree(&f.config), before);
    assert!(!f.manager.home().join("integrations").exists());
    assert!(
        !agentchat_runtime::daemon::RuntimePaths::for_project(&project)
            .socket
            .exists()
    );
    mcp.close().await;
}

#[tokio::test]
async fn current_health_changes_after_external_edits_even_when_init_replay_is_frozen() {
    let f = Fixture::new();
    let bin = path(&f, true);
    let id = Uuid::new_v4().to_string();
    fs::create_dir_all(f.config.join(".codex")).unwrap();
    fs::write(
        f.config.join(".codex/config.toml"),
        "model = 'PRIVATE_DIAGNOSTIC_SENTINEL'\n",
    )
    .unwrap();
    let initialized = command(&f, &["--json", "--request-id", &id, "install"], &bin).await;
    let project = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    let mut mcp = peer(&f, &bin).await;
    check(&f, &mut mcp, &bin, "whoami", "healthy").await;
    check(&f, &mut mcp, &bin, "doctor", "healthy").await;
    fs::write(
        f.config.join(".agents/skills/agentchat/SKILL.md"),
        "externally changed",
    )
    .unwrap();
    let before = tree(&f.config);
    let metadata = tree(&f.manager.home().join("integrations"));
    check(&f, &mut mcp, &bin, "whoami", "attention_required").await;
    check(&f, &mut mcp, &bin, "doctor", "attention_required").await;
    assert_eq!(
        command(&f, &["--json", "--request-id", &id, "install"], &bin).await,
        initialized
    );
    assert_eq!(tree(&f.config), before);
    assert_eq!(tree(&f.manager.home().join("integrations")), metadata);
    let lock = fs::File::open(f.manager.home().join("integrations/install.lock")).unwrap();
    fs2_lock(&lock);
    check(&f, &mut mcp, &bin, "whoami", "busy").await;
    drop(lock);
    let report = command(&f, &["--json", "doctor"], &bin).await;
    let health = serde_json::to_string(&report["result"]["integrations"]).unwrap();
    assert!(!health.contains("PRIVATE_DIAGNOSTIC_SENTINEL"));
    assert!(!health.contains(f.root.path().to_str().unwrap()));
    mcp.close().await;
    f.stop_lazy(&project).await;
}
fn fs2_lock(file: &fs::File) {
    // std's lock interoperates with the installer's fs2 flock on these OSes.
    file.lock().unwrap();
}
struct Fail;
impl InstallProgress for Fail {
    fn reached(&self, stage: InstallStage) -> Result<()> {
        if stage == InstallStage::Published(0) {
            Err(Error::new(ErrorCode::IoError, "test publication cut"))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn public_diagnostics_leave_real_pending_installation_for_explicit_init_recovery() {
    let f = Fixture::new();
    let project = f.initialize().await;
    let bin = path(&f, true);
    let setup = ToolSetup::new(
        f.manager.home().to_owned(),
        fs::canonicalize(env!("CARGO_BIN_EXE_agentchat")).unwrap(),
        Environment {
            user_config_root: Some(f.config.clone()),
            ..Environment::default()
        },
        Some(bin.clone().into_os_string()),
    )
    .with_progress(Arc::new(Fail));
    assert!(
        setup
            .install(Uuid::new_v4(), project.identity().id)
            .await
            .is_err()
    );
    let before = tree(&f.config);
    let metadata = tree(&f.manager.home().join("integrations"));
    let mut mcp = peer(&f, &bin).await;
    check(&f, &mut mcp, &bin, "doctor", "recovery_required").await;
    assert_eq!(tree(&f.config), before);
    assert_eq!(tree(&f.manager.home().join("integrations")), metadata);
    command(&f, &["--json", "install"], &bin).await;
    check(&f, &mut mcp, &bin, "doctor", "healthy").await;
    assert!(!f.manager.home().join("integrations/pending.json").exists());
    mcp.close().await;
}
