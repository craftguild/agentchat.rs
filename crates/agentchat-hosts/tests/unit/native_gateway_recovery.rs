//! Injected uncertainty and actual SIGKILL at every successful cleanup prefix.
use super::*;
use std::{
    fs,
    os::unix::{fs::DirBuilderExt, process::ExitStatusExt},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
#[path = "../gateway_recovery/harness.rs"]
mod harness;
use harness::{Gateway, OwnedChild};
#[path = "native_transport_loss.rs"]
mod transport_loss;

pub(super) fn cut(label: &str, root: &Path) {
    if std::env::var("AGENTCHAT_GATEWAY_RECOVERY_CUT")
        .ok()
        .as_deref()
        != Some(label)
        || std::env::var_os("AGENTCHAT_GATEWAY_RECOVERY_ROOT").as_deref() != Some(root.as_os_str())
    {
        return;
    }
    let marker = std::env::var_os("AGENTCHAT_GATEWAY_RECOVERY_MARKER").unwrap();
    fs::write(&marker, label).unwrap();
    fs::File::open(marker).unwrap().sync_all().unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn root(base: &Path) -> std::path::PathBuf {
    let root = base.join("metadata");
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    root
}
#[tokio::test]
#[ignore = "owned cleanup worker killed at an actual successful deletion prefix"]
async fn recovery_process_child() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AGENTCHAT_GATEWAY_RECOVERY_ROOT").unwrap());
    let scope = std::env::var("AGENTCHAT_GATEWAY_RECOVERY_SCOPE")
        .unwrap()
        .parse()
        .unwrap();
    let native =
        serde_json::from_str(&std::env::var("AGENTCHAT_GATEWAY_RECOVERY_NATIVE").unwrap()).unwrap();
    let lease = super::super::TransportLease::bind(&root, scope, &native)
        .await
        .unwrap();
    drop(lease);
}

#[tokio::test]
async fn actual_killed_cleanup_workers_leave_retryable_original_prefixes() {
    for label in ["socket", "directory", "endpoint"] {
        let base = tempfile::tempdir().unwrap();
        let root = root(base.path());
        let scope = Uuid::new_v4();
        let native = ProcessIdentity::capture(std::process::id()).unwrap();
        let mut gateway = Gateway::spawn(&root, scope, native.pid(), &base.path().join("ready"));
        assert!(gateway.inherited.is_none());
        gateway.kill();
        let record = root.join(format!("gateway-{scope}.json"));
        let bytes = fs::read(&record).unwrap();
        let marker = base.path().join("cut");
        let mut worker = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "claude::namespace::transport::recovery::tests::recovery_process_child",
                    "--nocapture",
                ])
                .env("AGENTCHAT_GATEWAY_RECOVERY_ROOT", &root)
                .env("AGENTCHAT_GATEWAY_RECOVERY_SCOPE", scope.to_string())
                .env(
                    "AGENTCHAT_GATEWAY_RECOVERY_NATIVE",
                    serde_json::to_string(&native).unwrap(),
                )
                .env("AGENTCHAT_GATEWAY_RECOVERY_CUT", label)
                .env("AGENTCHAT_GATEWAY_RECOVERY_MARKER", &marker)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while fs::read(&marker).ok().as_deref() != Some(label.as_bytes()) {
            assert!(worker.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "cleanup prefix {label}");
            std::thread::sleep(Duration::from_millis(5));
        }
        let physical = ProcessIdentity::capture(worker.0.id()).unwrap();
        worker.0.kill().unwrap();
        assert_eq!(worker.0.wait().unwrap().signal(), Some(9));
        assert_eq!(physical.birth_state(), BirthState::Gone);
        assert!(!gateway.endpoint.socket.exists());
        assert_eq!(
            gateway.endpoint.socket.parent().unwrap().exists(),
            label == "socket"
        );
        if label == "endpoint" {
            assert!(!record.exists());
        } else {
            assert_eq!(fs::read(&record).unwrap(), bytes);
        }
        let next = super::super::TransportLease::bind(&root, scope, &native)
            .await
            .unwrap();
        assert_ne!(next.nonce, gateway.endpoint.nonce);
        assert!(!gateway.endpoint.socket.parent().unwrap().exists());
        drop(next);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert!(native.is_current());
    }
}

#[tokio::test]
async fn uncertain_birth_and_same_birth_exec_preserve_until_positive_original_loss() {
    let base = tempfile::tempdir().unwrap();
    let path = root(base.path());
    let scope = Uuid::new_v4();
    let native = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut gateway = Gateway::spawn(&path, scope, native.pid(), &base.path().join("ready"));
    assert!(gateway.inherited.is_none());
    let record = path.join(format!("gateway-{scope}.json"));
    let bytes = fs::read(&record).unwrap();
    let directory = Directory::owned(&path).unwrap();
    let lock_name = format!("gateway-{scope}.lock");
    let snapshot = directory.read(lock_name.as_ref()).unwrap().unwrap();
    assert!(
        directory
            .lock_record(lock_name.as_ref(), &snapshot)
            .is_err()
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    gateway.exec();
    // An observed executable change is not a receipt that CLOEXEC teardown has
    // finished. Await the original lock itself, while keeping the same live
    // birth, unchanged inode/content and existing functional deadline mandatory.
    // Production recovery stays nonblocking and must still refuse this live birth.
    let lock = loop {
        assert_eq!(gateway.helper.birth_state(), BirthState::Present);
        assert!(directory.matches(lock_name.as_ref(), &snapshot).unwrap());
        assert_eq!(fs::read(&record).unwrap(), bytes);
        match directory.lock_record(lock_name.as_ref(), &snapshot) {
            Ok(lock) => break lock,
            Err(error) => {
                let file = fs::File::open(path.join(&lock_name)).unwrap();
                match fs2::FileExt::try_lock_exclusive(&file) {
                    Ok(()) => fs2::FileExt::unlock(&file).unwrap(),
                    Err(cause) => assert_eq!(
                        cause.kind(),
                        std::io::ErrorKind::WouldBlock,
                        "Original lock failed for another reason: {error}; {cause}"
                    ),
                }
                assert!(Instant::now() < deadline, "original exec lock teardown");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    assert!(recover(&directory, scope, &native, &snapshot, &mut |_| {}).is_err());
    assert_eq!(fs::read(&record).unwrap(), bytes);
    gateway.kill();
    assert!(
        recover_with(
            &directory,
            scope,
            &native,
            &snapshot,
            &mut |_| {},
            |token| {
                if token == &gateway.helper {
                    BirthState::Unknown
                } else {
                    token.birth_state()
                }
            }
        )
        .is_err()
    );
    assert_eq!(fs::read(&record).unwrap(), bytes);
    assert!(gateway.endpoint.socket.exists());
    recover(&directory, scope, &native, &snapshot, &mut |_| {}).unwrap();
    assert!(!record.exists());
    assert!(!gateway.endpoint.socket.parent().unwrap().exists());
    drop(lock);
}
