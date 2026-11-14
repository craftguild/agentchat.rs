//! Physical kernel-owner locators do not assert any native root or host admission.
use agentchat_hosts::claude::{namespace::OwnerNamespace, process::ProcessIdentity};
use std::{
    os::unix::fs::PermissionsExt,
    process::{Child, Command},
};
struct Owner(Child);
impl Owner {
    fn new() -> Self {
        Self(Command::new("/bin/sleep").arg("60").spawn().unwrap())
    }
    fn token(&self) -> ProcessIdentity {
        let expected = std::path::Path::new("/bin/sleep").canonicalize().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(owner) = ProcessIdentity::capture(self.0.id())
                && owner.executable().canonicalize().ok().as_ref() == Some(&expected)
                && owner.is_current()
            {
                return owner;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture image readiness"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
#[tokio::test]
async fn predecessor_cleanup_requires_the_exact_pending_successor_and_cannot_authorize_registration()
 {
    use agentchat_hosts::claude::{NativeBootstrap, NativeLifecycle};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let home = root();
    let owner = Owner::new();
    let ns = OwnerNamespace::open(home.path(), owner.token())
        .await
        .unwrap();
    let native_root = uuid::Uuid::new_v4();
    let lifecycle = |generation| {
        NativeLifecycle::Bootstrap(NativeBootstrap {
            owner_root: ns.id(),
            observer: ns.observer().into(),
            generation,
            root: native_root,
            lifecycle_id: uuid::Uuid::new_v4(),
            host_version: "2.1.301".into(),
            os_profile: "synthetic-only".into(),
            working_directory: home.path().into(),
        })
    };
    let first = ns
        .reserve(native_root, home.path().into(), None)
        .await
        .unwrap();
    ns.commit(first.clone(), lifecycle(first.generation))
        .await
        .unwrap();
    let prior = ns.read_current().await.unwrap().unwrap();
    let second = ns
        .reserve_after(first.generation, native_root, home.path().into(), None)
        .await
        .unwrap();
    let third = ns
        .reserve_after(second.generation, native_root, home.path().into(), None)
        .await
        .unwrap();
    let called = Arc::new(AtomicUsize::new(0));
    let count = called.clone();
    assert!(
        ns.clean_predecessor(second, prior.clone(), move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(called.load(Ordering::SeqCst), 0);
    let count = called.clone();
    ns.clean_predecessor(third.clone(), prior.clone(), move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(called.load(Ordering::SeqCst), 1);
    assert!(ns.read_current().await.unwrap().is_none());
    ns.commit(third.clone(), lifecycle(third.generation))
        .await
        .unwrap();
    let count = called.clone();
    assert!(
        ns.clean_predecessor(third, prior, move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(called.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn physical_owners_have_distinct_stable_namespaces_without_a_project_identity() {
    let r = root();
    let a = Owner::new();
    let b = Owner::new();
    let one = OwnerNamespace::open(r.path(), a.token()).await.unwrap();
    let same = OwnerNamespace::open(r.path(), a.token()).await.unwrap();
    let other = OwnerNamespace::open(r.path(), b.token()).await.unwrap();
    assert_eq!(one.directory(), same.directory());
    assert_ne!(one.directory(), other.directory());
    assert_eq!(one.id(), same.id());
    assert_ne!(one.id(), other.id());
    assert!(!one.id().is_nil());
    assert_eq!(one.observer(), same.observer());
    assert_eq!(one.owner(), &a.token());
    assert_eq!(one.directory().file_name().unwrap().len(), 64);
    assert!(!r.path().join("contexts").exists());
    assert!(!r.path().join("agentchat.sqlite3").exists());
    let names: std::collections::BTreeSet<_> = std::fs::read_dir(one.directory())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        [
            "owner-state.lock",
            "owner-state.json",
            "owner-state.initialized.json"
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect()
    );
    assert!(one.directory().join("owner-state.lock").is_file());
    assert!(one.directory().join("owner-state.json").is_file());
}
#[tokio::test]
async fn a_dead_owner_cannot_create_or_reopen_an_ipc_scope() {
    let r = root();
    let mut owner = Owner::new();
    let token = owner.token();
    let namespace = OwnerNamespace::open(r.path(), token.clone()).await.unwrap();
    let state = std::fs::read(namespace.directory().join("owner-state.json")).unwrap();
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    assert!(OwnerNamespace::open(r.path(), token).await.is_err());
    assert!(namespace.directory().is_dir());
    assert!(namespace.current().await.is_err());
    assert!(
        namespace
            .reserve(uuid::Uuid::new_v4(), "/".into(), None)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(namespace.directory().join("owner-state.json")).unwrap(),
        state
    );
}
#[tokio::test]
async fn symbolic_or_public_native_directories_refuse_without_writing_the_target() {
    let r = root();
    let target = root();
    std::os::unix::fs::symlink(target.path(), r.path().join("hosts")).unwrap();
    let owner = Owner::new();
    assert!(OwnerNamespace::open(r.path(), owner.token()).await.is_err());
    assert!(std::fs::read_dir(target.path()).unwrap().next().is_none());
    std::fs::remove_file(r.path().join("hosts")).unwrap();
    std::fs::create_dir(r.path().join("hosts")).unwrap();
    std::fs::set_permissions(
        r.path().join("hosts"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(OwnerNamespace::open(r.path(), owner.token()).await.is_err());
    assert!(
        std::fs::read_dir(r.path().join("hosts"))
            .unwrap()
            .next()
            .is_none()
    );
}
#[tokio::test]
async fn concurrent_helpers_reuse_the_exact_same_private_owner_scope() {
    let r = root();
    let owner = Owner::new();
    let token = owner.token();
    let results = futures_util::future::join_all(
        (0..8).map(|_| OwnerNamespace::open(r.path(), token.clone())),
    )
    .await;
    let first = results[0].as_ref().unwrap().directory();
    assert!(
        results
            .iter()
            .all(|x| x.as_ref().unwrap().directory() == first)
    );
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(first).unwrap().mode() & 0o777, 0o700);
}
