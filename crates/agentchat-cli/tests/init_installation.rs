//! Public CLI/MCP initialization uses the real preserving installation engine.
mod support;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration};
use support::{Fixture, mcp::Mcp};
use uuid::Uuid;

fn host(bin: &Path, name: &str) {
    let p = bin.join(name);
    fs::write(&p, "#!/bin/sh\nexit 77\n").unwrap();
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}
async fn cli(f: &Fixture, bin: &Path, id: Uuid) -> (i32, Value) {
    let child = f
        .command(&[
            "--json",
            "--request-id",
            &id.to_string(),
            "install",
            "--scope",
            "directory",
        ])
        .env("PATH", bin)
        .spawn()
        .unwrap();
    let out = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        out.status.code().unwrap(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}
async fn mcp(f: &Fixture, bin: &Path) -> Mcp {
    Mcp::from_child(
        f.command(&["mcp"])
            .env("PATH", bin)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    )
    .await
}
fn args(f: &Fixture, id: Uuid) -> Value {
    json!({"project_path":f.workspace,"scope":"directory","request_id":id})
}

#[tokio::test]
async fn public_setup_grammar_is_install_and_legacy_init_is_not_an_alias() {
    let f = Fixture::new();
    let help = f.raw(&["--help"], None).await;
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("install "))
    );
    assert!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("init "))
    );
    let help = f.raw(&["install", "--help"], None).await;
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    for flag in ["--scope", "--new-project", "--relocate", "--request-id"] {
        assert!(text.contains(flag), "missing {flag}: {text}");
    }
    let old = f.raw(&["init"], None).await;
    assert!(!old.status.success());
    assert!(!f.workspace.join(".agentchat").exists());
    assert!(!f.manager.home().exists());
}

#[tokio::test]
async fn legacy_completed_init_refuses_equally_without_rewriting_and_fresh_ordinary_init_keeps_uuid()
 {
    let f = Fixture::new();
    let bin = f.root.path().join("legacy-path");
    fs::create_dir(&bin).unwrap();
    host(&bin, "claude");
    let id = Uuid::new_v4();
    let (code, first) = cli(&f, &bin, id).await;
    assert_eq!(code, 0, "{first}");
    let receipt = f
        .manager
        .home()
        .join("integrations/requests")
        .join(format!("{id}.json"));
    let legacy = serde_json::to_vec(&json!({"version":1,"request":id,
        "namespace":first["project_id"],"hosts":["claude"]}))
    .unwrap();
    fs::write(&receipt, &legacy).unwrap();
    let registry = fs::read(f.config.join(".claude.json")).unwrap();
    let ledger = fs::read(f.manager.home().join("integrations/ownership.json")).unwrap();
    let (code, refused) = cli(&f, &bin, id).await;
    assert_ne!(code, 0);
    assert_eq!(refused["error"]["code"], "configuration_conflict");
    assert_eq!(
        refused["error"]["details"]["cause"]["reason"],
        "historical_installation_result_unavailable"
    );
    assert_eq!(
        refused["error"]["details"]["initialization"]["project_id"],
        first["project_id"]
    );
    let mut peer = mcp(&f, &bin).await;
    assert_eq!(peer.call("agentchat_init", args(&f, id)).await, refused);
    assert_eq!(fs::read(&receipt).unwrap(), legacy);
    assert_eq!(fs::read(f.config.join(".claude.json")).unwrap(), registry);
    assert_eq!(
        fs::read(f.manager.home().join("integrations/ownership.json")).unwrap(),
        ledger
    );
    let current = peer
        .call("agentchat_doctor", json!({"project_path":f.workspace}))
        .await;
    assert_eq!(current["project_id"], first["project_id"]);
    let (code, fresh) = cli(&f, &bin, Uuid::new_v4()).await;
    assert_eq!(code, 0, "{fresh}");
    assert_eq!(fresh["project_id"], first["project_id"]);
    assert_eq!(fresh["result"]["created"], false);
    assert_eq!(fs::read(&receipt).unwrap(), legacy);
    peer.close().await;
    let project = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    f.stop_lazy(&project).await;
}

#[tokio::test]
async fn both_transports_install_only_present_hosts_and_preserve_foreign_settings() {
    for names in [
        vec![],
        vec!["codex"],
        vec!["claude"],
        vec!["codex", "claude"],
    ] {
        let f = Fixture::new();
        let bin = f.root.path().join("path with spaces");
        fs::create_dir(&bin).unwrap();
        for name in &names {
            host(&bin, name);
        }
        fs::create_dir_all(f.config.join(".codex")).unwrap();
        fs::create_dir_all(f.config.join(".claude")).unwrap();
        let codex = "# retained\napproval_policy = 'on-request'\n[mcp_servers.foreign]\ncommand = 'foreign-tool'\n";
        let claude = r#"{"mcpServers":{"foreign":{"command":"foreign-tool"}},"projects":{"/foreign":{"hasTrustDialogAccepted":false}}}"#;
        let settings = br#"{"permissions":{"defaultMode":"plan"},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign"}]}]}}"#;
        fs::write(f.config.join(".codex/config.toml"), codex).unwrap();
        fs::write(f.config.join(".claude.json"), claude).unwrap();
        fs::write(f.config.join(".claude/settings.json"), settings).unwrap();
        let mut peer = mcp(&f, &bin).await;
        let id = Uuid::new_v4();
        let first = peer.call("agentchat_init", args(&f, id)).await;
        let (code, repeated) = cli(&f, &bin, id).await;
        assert_eq!(code, 0, "{repeated}");
        assert_eq!(first, repeated);
        assert_eq!(first["result"]["created"], true);
        assert_eq!(
            first["result"]["capabilities"],
            agentchat_hosts::capabilities()
        );
        assert_eq!(
            first["result"]["integrations"]["lifecycle"]["state"],
            "unavailable"
        );
        for (name, skill) in [
            ("codex", ".agents/skills/agentchat/SKILL.md"),
            ("claude", ".claude/skills/agentchat/SKILL.md"),
        ] {
            assert_eq!(
                first["result"]["integrations"]["hosts"][name]["state"],
                if names.contains(&name) {
                    "tools_installed"
                } else {
                    "not_installed"
                }
            );
            assert_eq!(f.config.join(skill).exists(), names.contains(&name));
        }
        let registry: Value =
            serde_json::from_slice(&fs::read(f.config.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(registry["mcpServers"]["foreign"]["command"], "foreign-tool");
        assert_eq!(
            registry["projects"]["/foreign"]["hasTrustDialogAccepted"],
            false
        );
        if names.contains(&"claude") {
            assert_eq!(
                registry["mcpServers"]["agentchat"]["command"],
                fs::canonicalize(env!("CARGO_BIN_EXE_agentchat"))
                    .unwrap()
                    .to_str()
                    .unwrap()
            );
            assert_eq!(registry["mcpServers"]["agentchat"]["args"], json!(["mcp"]));
            assert_eq!(
                registry["mcpServers"]["agentchat"]["env"]["AGENTCHAT_USER_CONFIG_ROOT"],
                f.config.to_str().unwrap()
            );
        } else {
            assert!(registry["mcpServers"].get("agentchat").is_none());
        }
        let text = fs::read_to_string(f.config.join(".codex/config.toml")).unwrap();
        assert!(text.starts_with(codex));
        assert_eq!(
            fs::read(f.config.join(".claude/settings.json")).unwrap(),
            settings
        );
        assert!(!f.config.join(".codex/hooks.json").exists());
        peer.close().await;
    }
}

#[tokio::test]
async fn failed_installation_reports_retained_project_and_same_key_recovers_without_recreation() {
    let f = Fixture::new();
    let bin = f.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    host(&bin, "claude");
    fs::create_dir_all(&f.config).unwrap();
    let foreign = br#"{"mcpServers":{"agentchat":{"command":"foreign-owned"}}}"#;
    fs::write(f.config.join(".claude.json"), foreign).unwrap();
    let mut peer = mcp(&f, &bin).await;
    let id = Uuid::new_v4();
    let failed = peer.call("agentchat_init", args(&f, id)).await;
    assert_eq!(failed["error"]["code"], "configuration_conflict");
    assert_eq!(
        failed["error"]["details"]["initialization"]["created"],
        true
    );
    assert_eq!(
        failed["error"]["details"]["integrations"]["state"],
        "failed"
    );
    let project = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    assert_eq!(failed["project_id"], project.identity().id.to_string());
    assert_eq!(fs::read(f.config.join(".claude.json")).unwrap(), foreign);
    assert!(!f.config.join(".claude/skills/agentchat").exists());
    // Only the fixture owner removes its deliberate collision before retry.
    fs::write(f.config.join(".claude.json"), b"{}").unwrap();
    let (code, success) = cli(&f, &bin, id).await;
    assert_eq!(code, 0, "{success}");
    assert_eq!(success["project_id"], failed["project_id"]);
    assert_eq!(success["result"]["created"], true);
    assert_eq!(peer.call("agentchat_init", args(&f, id)).await, success);
    let changed = peer
        .call(
            "agentchat_init",
            json!({"project_path":f.workspace,"scope":"repository","request_id":id}),
        )
        .await;
    assert_eq!(changed["error"]["code"], "idempotency_conflict");
    peer.close().await;
}

#[tokio::test]
async fn concurrent_cli_and_mcp_init_share_one_namespace_and_one_completed_setup_receipt() {
    let f = Fixture::new();
    let bin = f.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    host(&bin, "codex");
    let mut peer = mcp(&f, &bin).await;
    let id = Uuid::new_v4();
    let ((code, a), b) = tokio::join!(cli(&f, &bin, id), peer.call("agentchat_init", args(&f, id)));
    assert_eq!(code, 0, "{a}");
    assert_eq!(a, b);
    for directory in ["init/requests", "integrations/requests"] {
        assert_eq!(
            fs::read_dir(f.manager.home().join(directory))
                .unwrap()
                .count(),
            1
        );
    }
    let p = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    let store = f.manager.open(&p).await.unwrap();
    use agentchat_core::ports::Storage;
    assert_eq!(
        store
            .snapshot()
            .await
            .unwrap()
            .statistics()
            .await
            .unwrap()
            .agents,
        0
    );
    std::sync::Arc::try_unwrap(store)
        .ok()
        .unwrap()
        .close()
        .await
        .unwrap();
    peer.close().await;
}
