//! Managed preparation preserves ownership, native controls and init history.
use super::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};

#[path = "codex_startup/fixture.rs"]
mod fixture;
use fixture::Fixture;

#[tokio::test]
async fn managed_selection_follows_each_callers_path_without_rewriting_or_old_engine_fallback() {
    let a = Fixture::new("running");
    let project = Uuid::new_v4();
    a.tools(Uuid::new_v4(), project).await;
    a.setup
        .install_codex_selector(
            project,
            CodexProfile {
                executable: a.cli.clone(),
                version: "0.163.0".into(),
            },
        )
        .await
        .unwrap();
    let before = fs::read(a.config.join(".codex/config.toml")).unwrap();
    let api = a.api(false);
    let (first, _) = a
        .setup
        .prepare_codex_plan(project, a.setup.codex_launch_plan().await.unwrap())
        .await
        .unwrap();
    api.await.unwrap();
    let original = a.cli.canonicalize().unwrap();
    assert_eq!(first.profile().unwrap().executable, original);
    let b = Fixture::new("running");
    let next = ToolSetup::new(
        a.root.path().join("state"),
        a.root.path().join("agentchat"),
        a.setup.environment(),
        Some(b.root.path().as_os_str().into()),
    );
    fs::remove_file(&a.cli).unwrap();
    let api = b.api(false);
    let plan = next.codex_launch_plan().await.unwrap();
    assert_eq!(plan.cli(), b.cli.canonicalize().unwrap());
    let (selected, _) = next.prepare_codex_plan(project, plan).await.unwrap();
    api.await.unwrap();
    assert_eq!(
        selected.profile().unwrap().executable,
        b.cli.canonicalize().unwrap()
    );
    assert_eq!(first.profile().unwrap().executable, original);
    assert_eq!(
        fs::read(a.config.join(".codex/config.toml")).unwrap(),
        before
    );
    let missing = ToolSetup::new(
        a.root.path().join("state"),
        a.root.path().join("agentchat"),
        a.setup.environment(),
        Some(a.root.path().as_os_str().into()),
    );
    let error = missing.codex_launch_plan().await.err().unwrap();
    assert_eq!(error.details["reason"], "not_found_in_path");
    assert_eq!(
        fs::read(a.config.join(".codex/config.toml")).unwrap(),
        before
    );
}

#[tokio::test]
async fn stopped_native_daemon_starts_once_and_missing_selector_is_installed_without_init_replay() {
    let f = Fixture::new("stopped");
    let project = Uuid::new_v4();
    let request = Uuid::new_v4();
    let first = f.tools(request, project).await;
    let receipt = f
        .root
        .path()
        .join(format!("state/integrations/requests/{request}.json"));
    let original = fs::read(&receipt).unwrap();
    let foreign_claude = br#"{"mcpServers":{"foreign":{"command":"retained"}}}"#;
    fs::write(f.config.join(".claude.json"), foreign_claude).unwrap();
    let api = f.api(false);
    let plan = f.setup.codex_launch_plan().await.unwrap();
    assert!(plan.profile().is_none());
    let (plan, daemon) = f.setup.prepare_codex_plan(project, plan).await.unwrap();
    api.await.unwrap();
    assert_eq!(plan.profile().unwrap().executable, daemon.executable);
    assert_eq!(plan.profile().unwrap().version, daemon.version);
    assert_eq!(f.starts(), 1);
    assert_eq!(f.setup.install(request, project).await.unwrap(), first);
    assert_eq!(fs::read(&receipt).unwrap(), original);
    let config = fs::read_to_string(f.config.join(".codex/config.toml")).unwrap();
    assert!(config.starts_with("approval_policy='on-request'"));
    assert!(config.contains("command='retained'"));
    assert!(config.contains("AGENTCHAT_CODEX_MANAGED"));
    assert!(!config.contains("AGENTCHAT_CODEX_NATIVE_EXE"));
    assert_eq!(
        fs::read(f.config.join(".claude.json")).unwrap(),
        foreign_claude
    );
    let ledger = fs::read(f.root.path().join("state/integrations/ownership.json")).unwrap();
    let receipts = fs::read_dir(f.root.path().join("state/integrations/requests"))
        .unwrap()
        .count();
    f.setup.prepare_codex_plan(project, plan).await.unwrap();
    assert_eq!(f.starts(), 1);
    assert_eq!(
        fs::read(f.root.path().join("state/integrations/ownership.json")).unwrap(),
        ledger
    );
    assert_eq!(
        fs::read_dir(f.root.path().join("state/integrations/requests"))
            .unwrap()
            .count(),
        receipts
    );
}

#[tokio::test]
async fn running_tool_only_daemon_is_reused_without_start() {
    let f = Fixture::new("running");
    let project = Uuid::new_v4();
    f.tools(Uuid::new_v4(), project).await;
    let api = f.api(false);
    let plan = f.setup.codex_launch_plan().await.unwrap();
    f.setup.prepare_codex_plan(project, plan).await.unwrap();
    api.await.unwrap();
    assert_eq!(f.starts(), 0);
}

#[tokio::test]
async fn failed_inspection_delegates_once_to_native_start_then_reinspects() {
    for probe in [
        "{}",
        "not-json",
        "{\"status\":\"unknown\"}",
        "{\"status\":\"running\"}",
        "{\"status\":\"running\",\"status\":\"stopped\"}",
    ] {
        let f = Fixture::new("stopped");
        fs::write(f.cli.with_extension("probe"), probe).unwrap();
        assert!(Daemon::ensure_running(&f.cli).await.is_ok());
        assert_eq!(f.starts(), 1);
    }
    let f = Fixture::new("stopped");
    fs::remove_file(&f.cli).unwrap();
    assert!(Daemon::ensure_running(&f.cli).await.is_err());
    assert_eq!(f.starts(), 0);
}

#[tokio::test]
async fn startup_failure_or_no_running_reinspection_cannot_install_or_retry() {
    for mode in ["fail", "unchanged"] {
        let f = Fixture::new("stopped");
        let project = Uuid::new_v4();
        f.tools(Uuid::new_v4(), project).await;
        fs::write(f.cli.with_extension("mode"), mode).unwrap();
        let before = fs::read(f.config.join(".codex/config.toml")).unwrap();
        let plan = f.setup.codex_launch_plan().await.unwrap();
        let error = f
            .setup
            .prepare_codex_plan(project, plan)
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.details["stage"],
            if mode == "fail" { "start" } else { "reinspect" }
        );
        assert_eq!(f.starts(), 1);
        assert_eq!(
            fs::read(f.config.join(".codex/config.toml")).unwrap(),
            before
        );
        assert!(
            f.setup
                .codex_launch_plan()
                .await
                .unwrap()
                .profile()
                .is_none()
        );
    }
}

#[tokio::test]
async fn missing_live_api_leaves_native_selection_absent() {
    let f = Fixture::new("stopped");
    let project = Uuid::new_v4();
    f.tools(Uuid::new_v4(), project).await;
    let before = fs::read(f.config.join(".codex/config.toml")).unwrap();
    let api = f.api(true);
    let plan = f.setup.codex_launch_plan().await.unwrap();
    let error = f
        .setup
        .prepare_codex_plan(project, plan)
        .await
        .err()
        .unwrap();
    api.await.unwrap();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["missing_api"], "initialize");
    assert_eq!(
        fs::read(f.config.join(".codex/config.toml")).unwrap(),
        before
    );
    assert!(
        f.setup
            .codex_launch_plan()
            .await
            .unwrap()
            .profile()
            .is_none()
    );
    assert_eq!(f.starts(), 1);
}

#[tokio::test]
async fn isolated_or_foreign_configuration_refuses_before_any_host_effect() {
    for foreign in [false, true] {
        let f = Fixture::new("stopped");
        let project = Uuid::new_v4();
        f.tools(Uuid::new_v4(), project).await;
        if foreign {
            fs::write(
                f.config.join(".codex/config.toml"),
                "[mcp_servers.agentchat]\ncommand='foreign'\n",
            )
            .unwrap();
        }
        let before = fs::read(f.config.join(".codex/config.toml")).unwrap();
        assert!(f.setup.prepare_codex_launch(project).await.is_err());
        assert!(f.calls().is_empty());
        assert_eq!(
            fs::read(f.config.join(".codex/config.toml")).unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn a_concurrent_different_native_selector_is_never_replaced() {
    let f = Fixture::new("running");
    let project = Uuid::new_v4();
    f.tools(Uuid::new_v4(), project).await;
    let installed = CodexProfile {
        executable: f.cli.clone(),
        version: "0.163.0".into(),
    };
    let roots = f.setup.environment().resolve().unwrap();
    let state = f.root.path().join("state");
    Installer::new(
        &state,
        roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
    )
    .unwrap()
    .install(
        roots
            .codex_native_artifacts(&f.root.path().join("agentchat"), &state, &installed)
            .unwrap(),
    )
    .await
    .unwrap();
    let before = fs::read(f.config.join(".codex/config.toml")).unwrap();
    let changed = CodexProfile {
        version: "0.164.0".into(),
        ..installed
    };
    assert_eq!(
        f.setup
            .install_codex_selector(project, changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(
        fs::read(f.config.join(".codex/config.toml")).unwrap(),
        before
    );
    assert!(f.calls().is_empty());
}

#[tokio::test]
async fn an_existing_native_selector_must_match_the_running_daemon() {
    let f = Fixture::new("running");
    let project = Uuid::new_v4();
    f.tools(Uuid::new_v4(), project).await;
    let roots = f.setup.environment().resolve().unwrap();
    let state = f.root.path().join("state");
    Installer::new(
        &state,
        roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
    )
    .unwrap()
    .install(
        roots
            .codex_native_artifacts(
                &f.root.path().join("agentchat"),
                &state,
                &CodexProfile {
                    executable: f.cli.clone(),
                    version: "0.164.0".into(),
                },
            )
            .unwrap(),
    )
    .await
    .unwrap();
    let before = fs::read(f.config.join(".codex/config.toml")).unwrap();
    let plan = f.setup.codex_launch_plan().await.unwrap();
    assert_eq!(
        f.setup
            .prepare_codex_plan(project, plan)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(f.starts(), 0);
    assert_eq!(
        fs::read(f.config.join(".codex/config.toml")).unwrap(),
        before
    );
}

#[tokio::test]
async fn cancelling_start_never_retries_or_changes_native_selection() {
    let f = Fixture::new("stopped");
    fs::write(f.cli.with_extension("mode"), "block").unwrap();
    let cli = f.cli.clone();
    let task = tokio::spawn(async move { Daemon::ensure_running(&cli).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while f.starts() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(f.starts(), 1);
    assert_eq!(
        f.calls(),
        vec!["app-server daemon version", "app-server daemon start"]
    );
    assert!(!f.config.exists());
}
