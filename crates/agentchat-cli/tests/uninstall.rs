//! Actual CLI administrative commands share runtime removal with isolated roots.
mod support;
use serde_json::Value;
use support::Fixture;
use uuid::Uuid;

#[tokio::test]
async fn cli_local_global_reinstall_and_exact_retry_preserve_expected_references() {
    let f = Fixture::new();
    let (code, installed) = f.run(&["install"]).await;
    assert_eq!(code, 0);
    let first = installed["project_id"].clone();
    let project = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    let id = Uuid::new_v4().to_string();
    let (code, removed) = f.run(&["uninstall", "--request-id", &id]).await;
    assert_eq!(code, 0, "{removed}");
    assert_eq!(removed["result"]["scope"], "project");
    assert_eq!(removed["project_id"], first);
    assert!(!project.reference_path().exists());
    assert!(!project.data_dir().exists());
    let (code, fresh) = f.run(&["install"]).await;
    assert_eq!(code, 0);
    assert_ne!(fresh["project_id"], first);
    assert_eq!(f.run(&["uninstall", "--request-id", &id]).await.1, removed);
    let current = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    let (code, reset) = f.run(&["uninstall", "--global"]).await;
    assert_eq!(code, 0, "{reset}");
    assert_eq!(reset["result"]["scope"], "global");
    assert_eq!(reset["result"]["project_references_retained"], true);
    assert!(current.reference_path().exists());
    assert!(!f.manager.home().exists());
    let (code, error) = f.run(&["install"]).await;
    assert_ne!(code, 0);
    assert_eq!(error["error"]["code"], "project_binding_mismatch");
    assert_eq!(f.run(&["install", "--new-project"]).await.0, 0);
}

#[tokio::test]
async fn global_ignores_inherited_context_and_unusable_project_and_never_discovers_a_project() {
    let f = Fixture::new();
    let output = f
        .command(&[
            "--json",
            "--project",
            "/missing/uninstall-project",
            "uninstall",
            "--global",
        ])
        .env("AGENTCHAT_CONTEXT", Uuid::new_v4().to_string())
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["state"], "already_absent");
    assert!(!f.manager.home().exists());
    assert!(!f.workspace.join(".agentchat").exists());
    let output = f
        .command(&["--json", "uninstall"])
        .env("AGENTCHAT_CONTEXT", Uuid::new_v4().to_string())
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["state"], "already_absent");
    assert_eq!(f.run(&["uninstall", "--global", "--limit", "1"]).await.0, 2);
}
