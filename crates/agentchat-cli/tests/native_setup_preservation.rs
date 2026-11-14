//! Actual CLI/MCP init reuses an owned native plan without host execution.
mod support;
use agentchat_core::model::HostKind;
use agentchat_hosts::{claude::observer::NativeProfile, install::Installer, setup::Environment};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio, time::Duration};
use support::{Fixture, mcp::Mcp};
use uuid::Uuid;
#[tokio::test]
async fn both_public_transports_keep_native_installation_and_current_health_in_agreement() {
    for cli_first in [true, false] {
        let f = Fixture::new();
        let bin = f.root.path().join("fake-hosts");
        fs::create_dir(&bin).unwrap();
        let native = bin.join("claude");
        fs::write(&native, "#!/bin/sh\nexit 91\n").unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
        let roots = Environment {
            user_config_root: Some(f.config.clone()),
            ..Environment::default()
        }
        .resolve()
        .unwrap();
        fs::create_dir_all(roots.claude_settings.parent().unwrap()).unwrap();
        let foreign = json!({"permissions":{"defaultMode":"plan","ask":["Write"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign"}]}]}});
        fs::write(
            &roots.claude_settings,
            serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();
        let installer = Installer::new(
            f.manager.home(),
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )
        .unwrap();
        installer
            .install(
                roots
                    .claude_native_artifacts(
                        std::path::Path::new(env!("CARGO_BIN_EXE_agentchat")),
                        f.manager.home(),
                        &NativeProfile {
                            executable: native,
                            version: "2.1.301".into(),
                        },
                    )
                    .unwrap(),
            )
            .await
            .unwrap();
        let before = fs::read(&roots.claude_registry).unwrap();
        let settings_before = fs::read(&roots.claude_settings).unwrap();
        let mut mcp = Mcp::from_child(
            f.command(&["mcp"])
                .env("PATH", &bin)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        )
        .await;
        let request = Uuid::new_v4();
        let args = json!({"project_path":f.workspace,"scope":"directory","request_id":request});
        let cli = || async {
            let output = f
                .command(&[
                    "--json",
                    "--request-id",
                    &request.to_string(),
                    "install",
                    "--scope",
                    "directory",
                ])
                .env("PATH", &bin)
                .output();
            let out = tokio::time::timeout(Duration::from_secs(15), output)
                .await
                .unwrap()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert!(out.stderr.is_empty());
            serde_json::from_slice::<Value>(&out.stdout).unwrap()
        };
        if cli_first {
            let first = cli().await;
            let replay = mcp.call("agentchat_init", args).await;
            assert_eq!(first, replay);
        } else {
            let first = mcp.call("agentchat_init", args).await;
            let replay = cli().await;
            assert_eq!(first, replay);
        }
        assert_eq!(fs::read(&roots.claude_registry).unwrap(), before);
        assert_eq!(fs::read(&roots.claude_settings).unwrap(), settings_before);
        let status = mcp
            .call("agentchat_doctor", json!({"project_path":f.workspace}))
            .await;
        assert_eq!(status["error"], Value::Null, "{status}");
        assert_eq!(
            status["result"]["integrations"]["tools"]["state"], "healthy",
            "{status}"
        );
        assert_eq!(
            status["result"]["capabilities"],
            agentchat_hosts::capabilities()
        );
        assert_eq!(status["result"]["health"]["statistics"]["agents"], 0);
        let doctor = mcp
            .call("agentchat_doctor", json!({"project_path":f.workspace}))
            .await;
        assert_eq!(
            doctor["result"]["integrations"],
            status["result"]["integrations"]
        );
        assert_eq!(doctor["result"]["database"]["integrity"], "ok");
        let settings: Value =
            serde_json::from_slice(&fs::read(&roots.claude_settings).unwrap()).unwrap();
        assert_eq!(settings["permissions"], foreign["permissions"]);
        assert_eq!(settings["hooks"]["Stop"][0], foreign["hooks"]["Stop"][0]);
        let stop = settings["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[1]["hooks"][0]["asyncRewake"], true);
        assert!(
            stop[1]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("claude-input --mode notice")
        );
        mcp.close().await;
        let project = f.manager.discover(&f.workspace).await.unwrap().unwrap();
        f.stop_lazy(&project).await;
    }
}
