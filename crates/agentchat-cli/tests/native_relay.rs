//! Real helper subprocesses with a synthetic native parent, never host admission.
use agentchat_hosts::claude::{
    freshness::{Challenge, Snapshot},
    observer::{
        exchange,
        relay::{self, Question},
    },
    process::ProcessIdentity,
};
use agentchat_protocol::framing;
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::{Instant, timeout},
};
use uuid::Uuid;

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn command(path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentchat"));
    command
        .current_dir(path)
        .env_remove("AGENTCHAT_CONTEXT")
        .env("AGENTCHAT_HOME", path.join("unused-state"))
        .env("AGENTCHAT_USER_CONFIG_ROOT", path.join("unused-config"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}
async fn endpoint(path: &Path, child: &mut tokio::process::Child) {
    let end = Instant::now() + Duration::from_secs(5);
    while !path.join("fresh-observer.json").exists() {
        assert!(child.try_wait().unwrap().is_none() && Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn answer(question: &Question) -> Snapshot {
    Snapshot {
        nonce: question.nonce,
        relay_sequence: question.relay_sequence,
        native_sequence: 1,
        observer: "observer".into(),
        owner_root: Uuid::new_v4(),
        root: Uuid::new_v4(),
        generation: 1,
        epoch: 1,
        available: true,
        paused: true,
        pause_cause: None,
        approval: true,
        active_turns: vec![],
        guard_present: true,
        surfaces: vec!["terminal".into()],
        version: "synthetic".into(),
        unix: 0,
    }
}

#[tokio::test]
async fn native_stdio_helpers_exchange_without_project_discovery_or_cli_envelopes() {
    let root = root();
    let expected = std::env::current_exe().unwrap();
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut relay = command(root.path())
        .args(["internal", "claude-relay", "--directory"])
        .arg(root.path())
        .args(["--observer", "observer", "--native-executable"])
        .arg(expected)
        .spawn()
        .unwrap();
    let mut questions = BufReader::new(relay.stdout.take().unwrap());
    endpoint(root.path(), &mut relay).await;
    let r = root.path().to_owned();
    let nonce = Uuid::new_v4();
    let pid = owner.pid();
    let exchange =
        tokio::spawn(async move { exchange(&r, "observer", pid, &Challenge { nonce }).await });
    let question: Question = timeout(Duration::from_secs(2), framing::read(&mut questions))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(question.nonce, nonce);
    let snapshot = answer(&question);
    let mut responder = command(root.path())
        .args(["internal", "claude-answer", "--socket"])
        .arg(&question.reply_socket)
        .spawn()
        .unwrap();
    let mut input = responder.stdin.take().unwrap();
    input
        .write_all(&serde_json::to_vec(&snapshot).unwrap())
        .await
        .unwrap();
    drop(input);
    let result = timeout(Duration::from_secs(4), responder.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty());
    assert_eq!(
        serde_json::to_value(exchange.await.unwrap().unwrap()).unwrap(),
        serde_json::to_value(snapshot).unwrap()
    );
    relay::retire(root.path(), "observer", &owner)
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_secs(3), relay.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!root.path().join("fresh-observer.json").exists());
    assert!(
        !root.path().join("unused-state").exists() && !root.path().join("unused-config").exists()
    );
}

#[tokio::test]
async fn wrong_native_parent_refuses_before_publishing_or_discovering_a_project() {
    let root = root();
    let result = command(root.path())
        .args(["internal", "claude-relay", "--directory"])
        .arg(root.path())
        .args([
            "--observer",
            "observer",
            "--native-executable",
            env!("CARGO_BIN_EXE_agentchat"),
        ])
        .output()
        .await
        .unwrap();
    assert!(!result.status.success() && result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("native process owner"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn malformed_oversize_and_invalid_helper_arguments_keep_stdout_clean() {
    let root = root();
    for bytes in [Vec::new(), b"{}".to_vec(), vec![b'x'; 8193]] {
        let mut child = command(root.path())
            .args(["internal", "claude-answer", "--socket"])
            .arg(root.path().join("absent.sock"))
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let _ = input.write_all(&bytes).await;
        drop(input);
        let output = timeout(Duration::from_secs(4), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(!output.status.success() && output.stdout.is_empty() && !output.stderr.is_empty());
    }
    let result = command(root.path())
        .args(["internal", "claude-answer", "--unknown-option"])
        .output()
        .await
        .unwrap();
    assert!(!result.status.success() && result.stdout.is_empty() && !result.stderr.is_empty());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn a_native_answer_with_unclosed_stdin_has_a_finite_ingress_deadline() {
    let root = root();
    let mut child = command(root.path())
        .args(["internal", "claude-answer", "--socket"])
        .arg(root.path().join("absent.sock"))
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let started = Instant::now();
    assert!(
        !timeout(Duration::from_secs(4), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(started.elapsed() >= Duration::from_secs(2));
    drop(input);
    let mut error = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut error)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&error).contains("timed out"));
}
