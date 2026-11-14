//! Synthetic answers and actual owned kernel tokens; no native host admission.
use agentchat_hosts::claude::{
    freshness::{Challenge, Snapshot},
    observer::{
        exchange,
        relay::{self, Question},
    },
    process::ProcessIdentity,
};
use agentchat_protocol::framing;
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream},
    net::UnixStream,
    task::JoinHandle,
    time::{Instant, timeout},
};
use uuid::Uuid;

fn private_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
struct Owner(Child);
impl Drop for Owner {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}
async fn owner() -> (Owner, ProcessIdentity) {
    let child = Owner(Command::new("/bin/sleep").arg("60").spawn().unwrap());
    let expected = Path::new("/bin/sleep").canonicalize().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(token) = ProcessIdentity::capture(child.0.id())
            && token.executable().canonicalize().unwrap() == expected
        {
            return (child, token);
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
async fn start(
    root: &Path,
    token: ProcessIdentity,
) -> (
    JoinHandle<agentchat_core::Result<()>>,
    BufReader<DuplexStream>,
    serde_json::Value,
) {
    let (output, input) = tokio::io::duplex(4096);
    let path = root.to_owned();
    let task = tokio::spawn(async move { relay::serve(&path, "observer", token, output).await });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(bytes) = std::fs::read(root.join("fresh-observer.json")) {
            return (
                task,
                BufReader::new(input),
                serde_json::from_slice(&bytes).unwrap(),
            );
        }
        assert!(!task.is_finished() && Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn answer(q: &Question) -> Snapshot {
    Snapshot {
        nonce: q.nonce,
        relay_sequence: q.relay_sequence,
        native_sequence: 42,
        observer: "observer".into(),
        owner_root: Uuid::new_v4(),
        root: Uuid::new_v4(),
        generation: 7,
        epoch: 9,
        available: true,
        paused: true,
        pause_cause: None,
        approval: true,
        active_turns: vec!["actual-native-turn".into()],
        guard_present: true,
        surfaces: vec!["terminal".into()],
        version: "synthetic".into(),
        unix: 0,
    }
}
fn ask(
    root: &Path,
    token: &ProcessIdentity,
    nonce: Uuid,
) -> JoinHandle<agentchat_core::Result<Snapshot>> {
    let path = root.to_owned();
    let pid = token.pid();
    tokio::spawn(async move { exchange(&path, "observer", pid, &Challenge { nonce }).await })
}
async fn question(reader: &mut BufReader<DuplexStream>) -> Question {
    timeout(Duration::from_secs(2), framing::read(reader))
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
async fn abort(task: JoinHandle<agentchat_core::Result<()>>) {
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn real_private_roundtrip_preserves_native_facts_without_response_files() {
    let root = private_root();
    let (_child, token) = owner().await;
    let (task, mut native, endpoint) = start(root.path(), token.clone()).await;
    let nonce = Uuid::new_v4();
    let call = ask(root.path(), &token, nonce);
    let q = question(&mut native).await;
    assert_eq!(q.nonce, nonce);
    assert_eq!(q.relay_sequence, 1);
    let fields = serde_json::to_value(&q).unwrap();
    assert_eq!(fields.as_object().unwrap().len(), 3);
    assert!(fields.get("root").is_none() && fields.get("context_id").is_none());
    let response = answer(&q);
    relay::respond(&q.reply_socket, &response).await.unwrap();
    assert_eq!(
        serde_json::to_value(call.await.unwrap().unwrap()).unwrap(),
        serde_json::to_value(response).unwrap()
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    let directory = q.reply_socket.parent().unwrap().to_owned();
    abort(task).await;
    assert!(!root.path().join("fresh-observer.json").exists());
    assert!(!directory.exists());
    assert!(!Path::new(endpoint["socket"].as_str().unwrap()).exists());
}

#[tokio::test]
async fn wrong_nonce_sequence_observer_and_oversize_cannot_complete_the_pending_ask() {
    let root = private_root();
    let (_child, token) = owner().await;
    let (task, mut native, _) = start(root.path(), token.clone()).await;
    let call = ask(root.path(), &token, Uuid::new_v4());
    let q = question(&mut native).await;
    for field in 0..4 {
        let mut wrong = answer(&q);
        match field {
            0 => wrong.nonce = Uuid::new_v4(),
            1 => wrong.relay_sequence += 1,
            2 => wrong.observer = "other".into(),
            _ => wrong.version = "x".repeat(9000),
        }
        assert!(relay::respond(&q.reply_socket, &wrong).await.is_err());
        assert!(!call.is_finished());
    }
    relay::respond(&q.reply_socket, &answer(&q)).await.unwrap();
    assert!(call.await.unwrap().is_ok());
    assert!(relay::respond(&q.reply_socket, &answer(&q)).await.is_err());
    abort(task).await;
}

#[tokio::test]
async fn replacement_retires_the_old_transport_without_process_signals() {
    let root = private_root();
    let (_child, token) = owner().await;
    let (first, _old_native, old) = start(root.path(), token.clone()).await;
    let (output, input) = tokio::io::duplex(4096);
    let path = root.path().to_owned();
    let second_token = token.clone();
    let second =
        tokio::spawn(async move { relay::serve(&path, "observer", second_token, output).await });
    timeout(Duration::from_secs(3), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mut native = BufReader::new(input);
    let deadline = Instant::now() + Duration::from_secs(2);
    let new = loop {
        if let Ok(bytes) = std::fs::read(root.path().join("fresh-observer.json")) {
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if value["retire_token"] != old["retire_token"] {
                break value;
            }
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(
        !Path::new(old["socket"].as_str().unwrap())
            .parent()
            .unwrap()
            .exists()
    );
    let call = ask(root.path(), &token, Uuid::new_v4());
    let q = question(&mut native).await;
    assert_eq!(q.relay_sequence, 1);
    relay::respond(&q.reply_socket, &answer(&q)).await.unwrap();
    assert!(call.await.unwrap().is_ok());
    relay::retire(root.path(), "observer", &token)
        .await
        .unwrap();
    timeout(Duration::from_secs(2), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!Path::new(new["socket"].as_str().unwrap()).exists());
}

#[tokio::test]
async fn abort_cleanup_preserves_a_foreign_successor_publication() {
    let root = private_root();
    let (_child, token) = owner().await;
    let (task, _native, endpoint) = start(root.path(), token.clone()).await;
    let marker = root.path().join("successor");
    std::fs::write(&marker, b"foreign successor").unwrap();
    std::fs::rename(marker, root.path().join("fresh-observer.json")).unwrap();
    abort(task).await;
    assert_eq!(
        std::fs::read(root.path().join("fresh-observer.json")).unwrap(),
        b"foreign successor"
    );
    assert!(
        !Path::new(endpoint["socket"].as_str().unwrap())
            .parent()
            .unwrap()
            .exists()
    );
    assert!(
        relay::serve(root.path(), "observer", token, tokio::io::sink())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_truncated_and_oversize_requests_never_reach_the_native_stream() {
    let root = private_root();
    let (_child, token) = owner().await;
    let (task, mut native, endpoint) = start(root.path(), token).await;
    for value in [b"{}\n".to_vec(), b"{\"nonce\":".to_vec(), vec![b'x'; 4096]] {
        let mut client = UnixStream::connect(endpoint["socket"].as_str().unwrap())
            .await
            .unwrap();
        let _ = client.write_all(&value).await;
        let _ = client.shutdown().await;
        let mut byte = [0];
        let closed = timeout(Duration::from_secs(2), client.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)));
    }
    assert!(
        timeout(
            Duration::from_millis(100),
            framing::read::<_, Question>(&mut native)
        )
        .await
        .is_err()
    );
    abort(task).await;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        relay::serve(
            root.path(),
            "observer",
            ProcessIdentity::capture(std::process::id()).unwrap(),
            tokio::io::sink()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn kernel_owner_loss_stops_idle_transport_and_refuses_a_prepared_pending_answer() {
    for pending in [false, true] {
        let root = private_root();
        let (mut child, token) = owner().await;
        let (task, mut native, _) = start(root.path(), token.clone()).await;
        let work = if pending {
            let call = ask(root.path(), &token, Uuid::new_v4());
            let q = question(&mut native).await;
            Some((call, q))
        } else {
            None
        };
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(!token.is_current());
        if let Some((call, q)) = work {
            assert!(relay::respond(&q.reply_socket, &answer(&q)).await.is_err());
            assert!(call.await.unwrap().is_err());
        }
        timeout(Duration::from_secs(6), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!root.path().join("fresh-observer.json").exists());
    }
}

#[tokio::test]
async fn a_different_live_kernel_owner_cannot_retire_or_replace_the_transport() {
    let root = private_root();
    let (_first, owner) = owner().await;
    let (_second, other) = self::owner().await;
    let (task, mut native, endpoint) = start(root.path(), owner.clone()).await;
    let before = std::fs::read(root.path().join("fresh-observer.json")).unwrap();
    assert!(
        relay::retire(root.path(), "observer", &other)
            .await
            .is_err()
    );
    assert!(
        relay::serve(root.path(), "observer", other, tokio::io::sink())
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(root.path().join("fresh-observer.json")).unwrap(),
        before
    );
    assert!(Path::new(endpoint["socket"].as_str().unwrap()).exists());
    let call = ask(root.path(), &owner, Uuid::new_v4());
    let q = question(&mut native).await;
    relay::respond(&q.reply_socket, &answer(&q)).await.unwrap();
    assert!(call.await.unwrap().is_ok());
    abort(task).await;
}

#[tokio::test]
async fn a_partially_written_native_question_times_out_and_retires_the_stream() {
    let root = private_root();
    let (_child, owner) = owner().await;
    let (output, _stalled_native) = tokio::io::duplex(8);
    let path = root.path().to_owned();
    let token = owner.clone();
    let task = tokio::spawn(async move { relay::serve(&path, "observer", token, output).await });
    let end = Instant::now() + Duration::from_secs(2);
    while !root.path().join("fresh-observer.json").exists() {
        assert!(Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let call = ask(root.path(), &owner, Uuid::new_v4());
    let error = timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, agentchat_core::ErrorCode::IoError);
    assert!(call.await.unwrap().is_err());
    assert!(!root.path().join("fresh-observer.json").exists());
}
