//! Public launch grammar checks configuration before starting a native host.
mod support;
use agentchat_core::model::HostKind;
use serde_json::Value;
use support::Fixture;

fn failure(out: &std::process::Output) -> Value {
    if !out.stdout.is_empty() {
        assert!(out.stderr.is_empty(), "{out:?}");
        return serde_json::from_slice(&out.stdout).unwrap();
    }
    let text = std::str::from_utf8(&out.stderr).unwrap();
    assert!(text.starts_with("Request: "), "{text}");
    let field = |prefix: &str| text.lines().find_map(|line| line.strip_prefix(prefix));
    assert!(field("Message: ").is_some(), "{text}");
    assert_eq!(field("Retryable: "), Some("false"));
    // Normalize only the common fields after checking the plain error surface.
    serde_json::json!({
        "error": {"code": field("Error: ").expect("plain error code")},
        "project_id": field("Project: "),
    })
}

#[tokio::test]
async fn native_launch_help_is_offline_and_lists_the_two_hosts() {
    let f = Fixture::new();
    let out = f.raw(&["launch", "--help"], None).await;
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for expected in ["codex", "claude", "--resume", "--name"] {
        assert!(text.contains(expected), "{text}");
    }
    assert!(!f.manager.home().exists());
    assert_eq!(std::fs::read_dir(&f.config).unwrap().count(), 0);
}

#[tokio::test]
async fn native_launch_surface_eligibility_precedes_configuration_and_execution() {
    let f = Fixture::new();
    let project = f.initialize().await;
    for (args, host) in [
        (vec!["launch", "codex"], HostKind::Codex),
        (vec!["launch", "claude"], HostKind::Claude),
        (vec!["--json", "launch", "claude"], HostKind::Claude),
    ] {
        let out = f
            .command(&args)
            .env("AGENTCHAT_CONTEXT", uuid::Uuid::new_v4().to_string())
            .output()
            .await
            .unwrap();
        let value = failure(&out);
        agentchat_hosts::require_native_launch(host).unwrap();
        assert_eq!(out.status.code(), Some(3), "{value}");
        assert_eq!(value["error"]["code"], "configuration_conflict");
        assert_eq!(value["project_id"], project.identity().id.to_string());
        assert_eq!(std::fs::read_dir(&f.config).unwrap().count(), 0);
        assert!(!f.manager.home().join("hosts/launches").exists());
        assert!(
            !agentchat_runtime::daemon::RuntimePaths::for_project(&project)
                .socket
                .exists()
        );
    }
}

#[tokio::test]
async fn launch_rejects_pagination_and_retry_options_without_initialization() {
    let f = Fixture::new();
    let request = uuid::Uuid::new_v4().to_string();
    for args in [
        vec!["--limit", "1", "launch", "codex"],
        vec!["--request-id", &request, "launch", "claude"],
    ] {
        let out = f.raw(&args, None).await;
        let value = failure(&out);
        assert_eq!(out.status.code(), Some(2), "{value}");
        assert_eq!(value["error"]["code"], "invalid_argument");
        assert!(!f.manager.home().exists());
    }
}

#[tokio::test]
async fn malformed_claude_resume_and_reserved_names_fail_before_startup() {
    let f = Fixture::new();
    f.initialize().await;
    for (args, code) in [
        (
            vec!["launch", "claude", "--resume", "not-a-uuid"],
            "invalid_argument",
        ),
        (
            vec![
                "launch",
                "claude",
                "--resume",
                "00000000-0000-0000-0000-000000000000",
            ],
            "invalid_argument",
        ),
        (
            vec!["launch", "claude", "--name", "agentchat"],
            "invalid_name",
        ),
    ] {
        let out = f.raw(&args, None).await;
        let value = failure(&out);
        assert_eq!(value["error"]["code"], code, "{value}");
        assert!(!f.manager.home().join("hosts/launches").exists());
    }
}
