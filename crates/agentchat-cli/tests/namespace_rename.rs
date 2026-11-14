//! Legacy environment and machine names cannot select state in the fresh namespace.
mod support;
use std::fs;
use support::Fixture;

#[tokio::test]
async fn legacy_environment_neither_overrides_new_roots_nor_binds_a_context() {
    let f = Fixture::new();
    let legacy_home = f.root.path().join("old-state");
    let legacy_config = f.root.path().join("old-config");
    fs::create_dir(&legacy_home).unwrap();
    fs::create_dir(&legacy_config).unwrap();
    fs::write(legacy_home.join("sentinel"), b"legacy state").unwrap();
    fs::write(legacy_config.join("sentinel"), b"legacy config").unwrap();
    let project = f.initialize().await;
    let (runtime, task) = f.server(&project).await;
    let output = f
        .command(&["--json", "whoami"])
        .env("AC_HOME", &legacy_home)
        .env("AC_USER_CONFIG_ROOT", &legacy_config)
        .env("AC_CONTEXT", "invalid-legacy-context")
        .output()
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "invalid_argument");
    assert!(output.stderr.is_empty());
    let output = f
        .command(&["--json", "--by-user", "whoami"])
        .env("AC_HOME", &legacy_home)
        .env("AC_USER_CONFIG_ROOT", &legacy_config)
        .env("AC_CONTEXT", "invalid-legacy-context")
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["participation"], "unjoined");
    assert!(result["result"]["actor"].is_null());
    assert!(result["result"]["context_id"].is_null());
    assert!(result["result"]["native_delivery"].is_null());
    assert_eq!(result["result"]["statistics"]["agents"], 0);
    assert_eq!(
        fs::read(legacy_home.join("sentinel")).unwrap(),
        b"legacy state"
    );
    assert_eq!(
        fs::read(legacy_config.join("sentinel")).unwrap(),
        b"legacy config"
    );
    assert_eq!(fs::read_dir(&legacy_home).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&legacy_config).unwrap().count(), 1);
    assert_eq!(
        project.database_path().file_name().unwrap(),
        "agentchat.sqlite3"
    );
    runtime.shutdown();
    task.await.unwrap().unwrap();
}
