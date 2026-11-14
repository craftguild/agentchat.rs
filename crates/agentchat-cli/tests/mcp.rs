//! Actual stdio SDK subprocesses share runtime identity and idempotency with CLI.
mod support;
use serde_json::{Value, json};
use std::process::Stdio;
use support::{Fixture, mcp::Mcp};
use uuid::Uuid;

#[tokio::test]
async fn installed_product_tool_plan_runs_mcp_from_a_literal_binary_path() {
    use agentchat_core::model::HostKind;
    use agentchat_hosts::{install::Installer, setup::Environment};
    use std::{fs, os::unix::fs::PermissionsExt};
    let f = Fixture::new();
    let roots = Environment {
        user_config_root: Some(f.config.clone()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    let binary = f
        .config
        .join("agentchat 'literal' $(touch SHOULD_NOT_EXIST) `literal`");
    fs::copy(env!("CARGO_BIN_EXE_agentchat"), &binary).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    // Public installation normally follows an execution of the installed CLI.
    // This library-planned fixture first establishes ordinary offline startup
    // for its fresh copy, independently of the unchanged MCP request deadline.
    let started = std::time::Instant::now();
    let startup = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        tokio::process::Command::new(&binary)
            .arg("--version")
            .current_dir(&f.workspace)
            .env("AGENTCHAT_HOME", f.manager.home())
            .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
            .env_remove("AGENTCHAT_CONTEXT")
            .env("PATH", "")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("Copied fixture executable did not complete ordinary offline startup")
    .unwrap();
    assert!(startup.status.success() && startup.stderr.is_empty());
    assert!(
        String::from_utf8(startup.stdout)
            .unwrap()
            .lines()
            .any(|line| line == format!("agentchat {}", env!("CARGO_PKG_VERSION")))
    );
    assert!(!f.manager.home().exists());
    eprintln!("Copied fixture offline startup: {:?}", started.elapsed());
    let hosts = [HostKind::Codex, HostKind::Claude];
    let installer = Installer::new(f.manager.home(), roots.tool_roots(&hosts)).unwrap();
    let artifacts = || {
        roots
            .tool_artifacts(&hosts, &binary, f.manager.home())
            .unwrap()
    };
    assert_eq!(
        installer.install(artifacts()).await.unwrap().changed_files,
        4
    );
    assert_eq!(
        installer.install(artifacts()).await.unwrap().changed_files,
        0
    );
    let registry: Value =
        serde_json::from_slice(&fs::read(&roots.claude_registry).unwrap()).unwrap();
    let config = &registry["mcpServers"]["agentchat"];
    let mut command = tokio::process::Command::new(config["command"].as_str().unwrap());
    for arg in config["args"].as_array().unwrap() {
        command.arg(arg.as_str().unwrap());
    }
    for (key, value) in config["env"].as_object().unwrap() {
        command.env(key, value.as_str().unwrap());
    }
    command.env("AGENTCHAT_CONTEXT", Uuid::new_v4().to_string());
    command
        .current_dir(&f.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut mcp = Mcp::from_child(command.spawn().unwrap()).await;
    let tools = mcp.request("tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 23);
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["name"].as_str().unwrap().starts_with("agentchat_"))
    );
    let legacy = mcp
        .request(
            "tools/call",
            json!({"name":"ac_status","arguments":{"project_path":f.workspace}}),
        )
        .await;
    assert!(
        legacy.get("error").is_some() || legacy["result"]["isError"] == true,
        "{legacy}"
    );
    assert_eq!(
        mcp.call("agentchat_history", json!({})).await["error"]["code"],
        "invalid_argument"
    );
    assert_eq!(
        mcp.call("agentchat_history", json!({"project_path":f.workspace}))
            .await["error"]["code"],
        "not_initialized"
    );
    assert!(!f.workspace.join(".agentchat").exists());
    assert!(!f.workspace.join("SHOULD_NOT_EXIST").exists());
    assert!(!f.config.join("SHOULD_NOT_EXIST").exists());
    assert!(!roots.codex_hooks.exists());
    assert!(!roots.claude_settings.exists());
    mcp.close().await;
}

#[tokio::test]
async fn stdio_discovery_is_unbound_inert_and_contains_23_typed_tools() {
    let f = Fixture::new();
    let mut mcp = Mcp::start(&f).await;
    let tools = mcp.request("tools/list", json!({})).await;
    let tools = tools["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 23);
    for name in ["agentchat_channel_join", "agentchat_channel_leave"] {
        assert!(tools.iter().any(|tool| tool["name"] == name));
    }
    let error = mcp.call("agentchat_history", json!({})).await;
    assert_eq!(error["error"]["code"], "invalid_argument");
    let error = mcp
        .call("agentchat_history", json!({"project_path":f.workspace}))
        .await;
    assert_eq!(error["error"]["code"], "not_initialized");
    assert!(!f.manager.home().exists());
    assert!(!f.workspace.join(".agentchat").exists());
    mcp.close().await;
    assert!(std::fs::read_dir(&f.config).unwrap().next().is_none());
}

#[tokio::test]
async fn executable_mcp_private_native_routing_preserves_successful_keys_and_explicit_receipts() {
    let f = Fixture::new();
    let project = f.initialize().await;
    let (runtime, task) = f.server(&project).await;
    let alice = f.context("mcp-alice").await;
    let bob = f.context("mcp-bob").await;
    let join_key = Uuid::new_v4().to_string();
    let mut mcp = Mcp::start(&f).await;
    let joined = mcp.call("agentchat_join", json!({"project_path":f.workspace,"context_id":alice.context_id,"name":"alice","request_id":join_key})).await;
    assert!(joined.get("result").is_some(), "{joined}");
    let replay = mcp
        .call(
            "agentchat_join",
            json!({"name":"alice","request_id":join_key}),
        )
        .await;
    assert_eq!(replay, joined);
    let mut recipient = Mcp::start(&f).await;
    assert!(
        recipient
            .call(
                "agentchat_join",
                json!({"project_path":f.workspace,"context_id":bob.context_id,"name":"bob"})
            )
            .await
            .get("result")
            .is_some()
    );
    let key = Uuid::new_v4().to_string();
    let body = "@bob @missing literal $(touch MCP_SHOULD_NOT_EXIST) `echo no`\n全文\n";
    let posted = mcp
        .call("agentchat_post", json!({"body":body,"request_id":key}))
        .await;
    assert!(posted.get("result").is_some(), "{posted}");
    assert_eq!(posted["result"]["warnings"][0]["code"], "unknown_recipient");
    assert!(!f.workspace.join("MCP_SHOULD_NOT_EXIST").exists());
    let root = posted["result"]["message_id"].as_str().unwrap();
    let message = mcp
        .call("agentchat_message", json!({"message_id":root}))
        .await;
    assert_eq!(message["result"]["message"]["body"], body.trim());
    let readers = f.run(&["message", root]).await.1;
    assert!(
        readers["result"]["receipts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["reads"].as_array().unwrap().is_empty())
    );
    assert!(
        recipient
            .call("agentchat_read", json!({"message_ids":[root]}))
            .await
            .get("result")
            .is_some()
    );
    let readers = f.run(&["message", root]).await.1;
    assert_eq!(
        readers["result"]["receipts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| !row["reads"].as_array().unwrap().is_empty())
            .count(),
        1
    );
    assert_eq!(
        f.run(&["history"]).await.1["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let switched = mcp
        .call("agentchat_join", json!({"context_id":bob.context_id}))
        .await;
    assert_eq!(switched["error"]["code"], "identity_conflict");
    let other = Fixture::new();
    other.initialize().await;
    let switched = mcp
        .call("agentchat_whoami", json!({"project_path":other.workspace}))
        .await;
    assert_eq!(switched["error"]["code"], "project_binding_mismatch");
    mcp.close().await;
    recipient.close().await;
    runtime.shutdown();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn restore_is_explicit_never_joins_and_failure_does_not_pollute_stdout() {
    let f = Fixture::new();
    let invalid = f
        .raw(&["--json", "mcp", "--restore-context", "invalid"], None)
        .await;
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    assert!(!invalid.stderr.is_empty());
    let project = f.initialize().await;
    let (runtime, task) = f.server(&project).await;
    let unjoined = f.context("new-unjoined").await.context_id.to_string();
    let output = f.raw(&["mcp", "--restore-context", &unjoined], None).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    let agents = f.run(&["agents", "list"]).await.1;
    assert!(agents["result"]["items"].as_array().unwrap().is_empty());
    runtime.shutdown();
    task.await.unwrap().unwrap();
}
