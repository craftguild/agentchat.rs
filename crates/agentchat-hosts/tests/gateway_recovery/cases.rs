//! Normal behavior and conservative refusal around real helper lifetimes.
use super::*;
use harness::{OwnedSocketDirectory, identity};
use std::{os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    gateway: Gateway,
    root: PathBuf,
    scope: Uuid,
    owner: ProcessIdentity,
    base: tempfile::TempDir,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        Self::with_owner(mode, ProcessIdentity::capture(std::process::id()).unwrap())
    }
    fn with_owner(mode: &str, owner: ProcessIdentity) -> Self {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("metadata");
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let scope = Uuid::new_v4();
        let gateway =
            Gateway::spawn_mode(&root, scope, owner.pid(), &base.path().join("ready"), mode);
        Self {
            gateway,
            root,
            scope,
            owner,
            base,
        }
    }
    fn record(&self) -> PathBuf {
        self.root.join(format!("gateway-{}.json", self.scope))
    }
    async fn bind(&self) -> agentchat_core::Result<TransportLease> {
        TransportLease::bind(&self.root, self.scope, &self.owner).await
    }
}

#[tokio::test]
async fn a_different_live_native_owner_cannot_collect_until_the_original_birth_is_gone() {
    let mut native = OwnedChild(
        Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let expected = Path::new("/bin/cat").canonicalize().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let original = loop {
        if let Ok(token) = ProcessIdentity::capture(native.0.id())
            && token.executable().canonicalize().unwrap() == expected
            && token.is_current()
        {
            break token;
        }
        assert!(native.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "original native image readiness");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let mut f = Fixture::with_owner("normal", original.clone());
    f.gateway.kill();
    let next_native = ProcessIdentity::capture(std::process::id()).unwrap();
    let bytes = fs::read(f.record()).unwrap();
    let id = identity(&f.gateway.endpoint.socket);
    assert!(
        TransportLease::bind(&f.root, f.scope, &next_native)
            .await
            .is_err()
    );
    assert_eq!(fs::read(f.record()).unwrap(), bytes);
    assert_eq!(identity(&f.gateway.endpoint.socket), id);
    drop(native.0.stdin.take());
    assert!(native.0.wait().unwrap().success());
    assert_eq!(original.birth_state(), BirthState::Gone);
    let lease = TransportLease::bind(&f.root, f.scope, &next_native)
        .await
        .unwrap();
    assert!(!f.gateway.endpoint.socket.parent().unwrap().exists());
    drop(lease);
    assert!(next_native.is_current());
}

#[tokio::test]
async fn normal_helper_exit_cleans_before_a_new_helper_uses_the_same_lock() {
    let mut f = Fixture::new("normal");
    let lock = f.root.join(format!("gateway-{}.lock", f.scope));
    let original = identity(&lock);
    drop(f.gateway.child.0.stdin.take());
    assert!(f.gateway.child.0.wait().unwrap().success());
    assert_eq!(f.gateway.helper.birth_state(), BirthState::Gone);
    assert!(!f.record().exists());
    assert!(!f.gateway.endpoint.socket.parent().unwrap().exists());
    let next = f.bind().await.unwrap();
    assert_ne!(next.nonce, f.gateway.endpoint.nonce);
    drop(next);
    assert_eq!(identity(&lock), original);
    assert!(f.owner.is_current());
}

#[tokio::test]
async fn same_birth_helper_exec_preserves_original_objects_until_actual_exit() {
    let mut f = Fixture::new("normal");
    let before = fs::read(f.record()).unwrap();
    let socket = identity(&f.gateway.endpoint.socket);
    f.gateway.exec();
    assert!(f.bind().await.is_err());
    assert_eq!(fs::read(f.record()).unwrap(), before);
    assert_eq!(identity(&f.gateway.endpoint.socket), socket);
    f.gateway.kill();
    let next = f.bind().await.unwrap();
    assert!(!f.gateway.endpoint.socket.parent().unwrap().exists());
    drop(next);
}

#[tokio::test]
async fn an_actual_descendant_keeps_the_original_listener_after_helper_death() {
    let mut f = Fixture::new("inherit");
    let inherited = f.gateway.inherited.clone().unwrap();
    let input = f.gateway.child.0.stdin.take().unwrap();
    let before = fs::read(f.record()).unwrap();
    let socket = identity(&f.gateway.endpoint.socket);
    f.gateway.kill();
    assert!(inherited.is_current());
    assert!(f.bind().await.is_err());
    assert_eq!(identity(&f.gateway.endpoint.socket), socket);
    assert_eq!(fs::read(f.record()).unwrap(), before);
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(20);
    while inherited.birth_state() != BirthState::Gone {
        assert!(Instant::now() < deadline, "owned inherited listener EOF");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let next = f.bind().await.unwrap();
    assert!(!f.gateway.endpoint.socket.parent().unwrap().exists());
    drop(next);
}

#[tokio::test]
async fn changed_records_and_external_objects_preserve_foreign_data_and_recovery_witnesses() {
    for kind in [
        "bytes",
        "alias",
        "copy",
        "root",
        "socket",
        "directory",
        "foreign",
        "version",
        "unknown",
    ] {
        let mut f = Fixture::new("normal");
        f.gateway.kill();
        let record = f.record();
        let socket = f.gateway.endpoint.socket.clone();
        let directory = socket.parent().unwrap().to_owned();
        let mut listener = None;
        let mut holder = None;
        let mut replacement = None;
        match kind {
            "bytes" => fs::write(&record, b"foreign changed endpoint bytes").unwrap(),
            "alias" => fs::hard_link(&record, f.root.join("alias")).unwrap(),
            "copy" => {
                let copy = f.root.join("copy");
                fs::copy(&record, &copy).unwrap();
                fs::rename(copy, &record).unwrap();
            }
            "root" => {
                let moved = f.base.path().join("original-root");
                fs::rename(&f.root, &moved).unwrap();
                fs::DirBuilder::new().mode(0o700).create(&f.root).unwrap();
                fs::rename(moved.join(record.file_name().unwrap()), &record).unwrap();
            }
            "socket" => {
                fs::remove_file(&socket).unwrap();
                listener = Some(tokio::net::UnixListener::bind(&socket).unwrap());
                fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
            }
            "directory" => {
                holder = Some(tempfile::tempdir_in(directory.parent().unwrap()).unwrap());
                let moved = holder.as_ref().unwrap().path().join("original");
                fs::rename(&directory, &moved).unwrap();
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .unwrap();
                replacement = Some(OwnedSocketDirectory(
                    directory.clone(),
                    identity(&directory),
                ));
                fs::rename(moved.join("control.sock"), &socket).unwrap();
            }
            "foreign" => fs::write(directory.join("foreign-note"), b"preserve").unwrap(),
            "version" | "unknown" => {
                let mut value = serde_json::to_value(&f.gateway.endpoint).unwrap();
                if kind == "version" {
                    value["ownership"]["version"] = 999.into();
                } else {
                    value["ownership"]["helper"]["boot"] = Uuid::nil().to_string().into();
                }
                // Deliberately publish a well-framed fixture to exercise the
                // inner version/birth validation, not only checksum refusal.
                metadata::write(&record, &value).unwrap();
            }
            _ => unreachable!(),
        }
        let bytes = fs::read(&record).unwrap();
        let record_id = identity(&record);
        let socket_id = identity(&socket);
        let directory_id = identity(&directory);
        assert!(f.bind().await.is_err(), "{kind}");
        assert_eq!(fs::read(&record).unwrap(), bytes, "{kind}");
        assert_eq!(identity(&record), record_id, "{kind}");
        assert_eq!(identity(&directory), directory_id, "{kind}");
        if kind == "foreign" {
            assert_eq!(
                fs::read(directory.join("foreign-note")).unwrap(),
                b"preserve"
            );
            assert!(!socket.exists());
            fs::remove_file(directory.join("foreign-note")).unwrap();
            let next = f.bind().await.unwrap();
            assert!(!directory.exists());
            drop(next);
        } else {
            assert_eq!(identity(&socket), socket_id, "{kind}");
        }
        if kind == "alias" {
            assert_eq!(fs::read(f.root.join("alias")).unwrap(), bytes);
        }
        if let Some(listener) = listener {
            let _peer = tokio::net::UnixStream::connect(&socket).await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), listener.accept())
                .await
                .unwrap()
                .unwrap();
        }
        drop(replacement);
        drop(holder);
    }
}
