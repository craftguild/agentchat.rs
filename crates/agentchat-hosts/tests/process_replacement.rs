//! Disposable process replacement; no native conversation or operational image.
use agentchat_core::model::LaunchOwnerState;
use agentchat_hosts::claude::process::{BirthState, ContinuityState, ProcessIdentity};
use agentchat_hosts::launch::LaunchOwners;
use std::{fs, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

async fn echo(child: &mut tokio::process::Child, bytes: &[u8]) {
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(bytes)
        .await
        .unwrap();
    let mut result = vec![0; bytes.len()];
    tokio::time::timeout(
        Duration::from_secs(3),
        child.stdout.as_mut().unwrap().read_exact(&mut result),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, bytes);
}

#[tokio::test]
async fn replacing_a_disposable_installed_image_does_not_mean_the_original_process_exited() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let installed = root.join("agentchat");
    let state = root.join("state");
    let config = root.join("config");
    fs::create_dir(&state).unwrap();
    fs::create_dir(&config).unwrap();
    let built = root.join("echo-helper");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/process_replacement/echo.rs");
    assert!(
        Command::new("rustc")
            .args(["--edition=2024", "-o"])
            .arg(&built)
            .arg(source)
            .status()
            .await
            .unwrap()
            .success()
    );
    fs::copy(&built, &installed).unwrap();
    let mut child = Command::new(&installed)
        .env("AGENTCHAT_HOME", &state)
        .env("AGENTCHAT_USER_CONFIG_ROOT", &config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    echo(&mut child, b"before replacement\n").await;
    let original = ProcessIdentity::capture(child.id().unwrap()).unwrap();
    assert!(original.is_current());
    assert!(original.matches_executable(&installed, installed.metadata().unwrap()));
    let mut supervisor = Command::new(&installed)
        .env("AGENTCHAT_HOME", &state)
        .env("AGENTCHAT_USER_CONFIG_ROOT", &config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    echo(&mut supervisor, b"original supervisor\n").await;
    let launch = LaunchOwners::capture(child.id().unwrap(), supervisor.id().unwrap()).unwrap();
    assert_eq!(LaunchOwners::observe(&launch), LaunchOwnerState::Current);
    for index in 0..3 {
        let stage = root.join(format!("replacement-{index}"));
        fs::copy(&built, &stage).unwrap();
        fs::rename(stage, &installed).unwrap();
        echo(&mut child, b"after replacement\n").await;
        echo(&mut supervisor, b"supervisor after replacement\n").await;
        assert!(child.try_wait().unwrap().is_none());
        assert_eq!(original.birth_state(), BirthState::Present);
        assert!(
            !original.is_current(),
            "strict initial-image proof unexpectedly survived replacement"
        );
        assert_eq!(original.continuity_state(), ContinuityState::Current);
        assert_eq!(LaunchOwners::observe(&launch), LaunchOwnerState::Current);
        println!(
            "replacement {index}: original continuity current, strict initial-image check false"
        );
    }
    fs::remove_file(&installed).unwrap();
    echo(&mut child, b"without an installed image path\n").await;
    assert_eq!(original.continuity_state(), ContinuityState::Current);
    let retained: ProcessIdentity =
        serde_json::from_value(serde_json::to_value(&original).unwrap()).unwrap();
    assert_eq!(retained, original);
    assert!(retained.is_continuing());
    assert_eq!(LaunchOwners::observe(&launch), LaunchOwnerState::Current);
    drop(supervisor.stdin.take());
    assert!(supervisor.wait().await.unwrap().success());
    echo(&mut child, b"native survives original supervisor exit\n").await;
    assert_eq!(LaunchOwners::observe(&launch), LaunchOwnerState::Gone);
    drop(child.stdin.take());
    assert!(child.wait().await.unwrap().success());
    assert_eq!(original.birth_state(), BirthState::Gone);
    assert_eq!(original.continuity_state(), ContinuityState::Gone);
}
