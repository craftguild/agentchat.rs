//! Public init and the generated MCP entry use the same isolated native roots.
mod support;

use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio};
use support::{Fixture, mcp::Mcp};
use uuid::Uuid;

#[tokio::test]
async fn public_cli_installation_and_uninitialized_mcp_init_share_native_config_and_health() {
    let f = Fixture::new();
    let native = f.config.join("native Claude");
    fs::create_dir(&native).unwrap();
    let bin = f.root.path().join("owned-bin");
    fs::create_dir(&bin).unwrap();
    let host = bin.join("claude");
    fs::write(&host, "#!/bin/sh\nexit 77\n").unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
    let foreign = json!({"mcpServers":{"foreign":{"command":"retained"}},"projects":{"/foreign":{"hasTrustDialogAccepted":false}}});
    fs::write(native.join(".claude.json"), foreign.to_string()).unwrap();
    let request = Uuid::new_v4().to_string();
    let output = f
        .command(&["--json", "install", "--request-id", &request])
        .env("CLAUDE_CONFIG_DIR", &native)
        .env("PATH", &bin)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let initial: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        initial["result"]["integrations"]["hosts"]["claude"]["state"],
        "tools_installed"
    );
    let registry: Value =
        serde_json::from_slice(&fs::read(native.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(registry["projects"], foreign["projects"]);
    assert_eq!(
        registry["mcpServers"]["foreign"],
        foreign["mcpServers"]["foreign"]
    );
    let entry = &registry["mcpServers"]["agentchat"];
    assert_eq!(entry["command"], env!("CARGO_BIN_EXE_agentchat"));
    assert_eq!(entry["args"], json!(["mcp"]));
    assert_eq!(entry["env"]["CLAUDE_CONFIG_DIR"], native.to_str().unwrap());
    let mut command = f.command(&["mcp"]);
    command
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("PATH", &bin)
        .stdin(Stdio::piped());
    for (name, value) in entry["env"].as_object().unwrap() {
        command.env(name, value.as_str().unwrap());
    }
    let mut mcp = Mcp::from_child(command.spawn().unwrap()).await;
    let first = mcp
        .call("agentchat_doctor", json!({"project_path":f.workspace}))
        .await;
    assert_eq!(first["result"]["integrations"]["tools"]["state"], "healthy");
    assert_eq!(first["result"]["health"]["statistics"]["agents"], 0);
    let next = f.root.path().join("uninitialized");
    fs::create_dir(&next).unwrap();
    assert_eq!(
        mcp.call("agentchat_doctor", json!({"project_path":next}))
            .await["error"]["code"],
        "not_initialized"
    );
    assert!(!next.join(".agentchat").exists());
    let key = Uuid::new_v4();
    let arguments = json!({"project_path":next,"request_id":key});
    let initialized = mcp.call("agentchat_init", arguments.clone()).await;
    assert!(initialized.get("result").is_some(), "{initialized}");
    assert_ne!(initialized["project_id"], initial["project_id"]);
    assert_eq!(mcp.call("agentchat_init", arguments).await, initialized);
    let status = mcp
        .call("agentchat_doctor", json!({"project_path":next}))
        .await;
    assert_eq!(
        status["result"]["integrations"]["tools"]["state"],
        "healthy"
    );
    assert_eq!(status["result"]["health"]["statistics"]["agents"], 0);
    assert_eq!(status["result"]["health"]["statistics"]["messages"], 0);
    assert_eq!(
        status["result"]["capabilities"],
        agentchat_hosts::capabilities()
    );
    assert!(!native.join("settings.json").exists());
    assert!(!f.config.join(".claude.json").exists());
    mcp.close().await;
    for directory in [&f.workspace, &next] {
        let project = f.manager.discover(directory).await.unwrap().unwrap();
        f.stop_lazy(&project).await;
    }
}
