//! Relative invocation must install the real absolute executable in both transports.
mod support;

use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink, process::Stdio};
use support::{Fixture, mcp::Mcp};
use uuid::Uuid;

fn executable(f: &Fixture) -> std::path::PathBuf {
    let bin = f.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    symlink(env!("CARGO_BIN_EXE_agentchat"), bin.join("agentchat")).unwrap();
    bin
}

fn installed_command(f: &Fixture) -> String {
    let registry: Value =
        serde_json::from_slice(&fs::read(f.config.join(".claude.json")).unwrap()).unwrap();
    registry["mcpServers"]["agentchat"]["command"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn hosts(bin: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    for name in ["codex", "claude"] {
        let file = bin.join(name);
        fs::write(&file, "#!/bin/sh\nexit 77\n").unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn relative(f: &Fixture, bin: &std::path::Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("../bin/agentchat");
    command
        .current_dir(&f.workspace)
        .env("AGENTCHAT_HOME", f.manager.home())
        .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
        .env_remove("AGENTCHAT_CONTEXT")
        .env("PATH", bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

#[tokio::test]
async fn relative_cli_init_installs_canonical_executable_and_retries_the_same_project() {
    let f = Fixture::new();
    let bin = executable(&f);
    hosts(&bin);
    let request = Uuid::new_v4().to_string();
    let mut command = relative(&f, &bin);
    command.args([
        "--json",
        "--request-id",
        &request,
        "install",
        "--new-project",
    ]);
    let first = tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
        .await
        .unwrap()
        .unwrap();
    let value: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert!(first.status.success(), "{value}");
    let actual = fs::canonicalize(env!("CARGO_BIN_EXE_agentchat")).unwrap();
    assert_eq!(installed_command(&f), actual.to_str().unwrap());
    let mut peer = Mcp::from_child(
        f.command(&["mcp"])
            .env("PATH", &bin)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    )
    .await;
    let repeated = peer
        .call(
            "agentchat_init",
            json!({"project_path":f.workspace,"request_id":request,"new_project":true}),
        )
        .await;
    assert_eq!(value, repeated);
    peer.close().await;
}

#[tokio::test]
async fn relative_mcp_init_installs_the_same_canonical_executable() {
    let f = Fixture::new();
    let bin = executable(&f);
    hosts(&bin);
    let mut command = relative(&f, &bin);
    command.arg("mcp");
    let mut peer = Mcp::from_child(command.spawn().unwrap()).await;
    let result = peer
        .call("agentchat_init", json!({"project_path":f.workspace}))
        .await;
    assert!(result.get("result").is_some(), "{result}");
    assert_eq!(
        installed_command(&f),
        fs::canonicalize(env!("CARGO_BIN_EXE_agentchat"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    peer.close().await;
}
