//! Real gateway cleanup must preserve unknown children and substituted parents.
use agentchat_cli::claude_gateway::records::{self, Endpoint};
#[path = "../src/claude_gateway/endpoint.rs"]
mod endpoint;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;
async fn bind(path: &std::path::Path, scope: Uuid) -> agentchat_core::Result<endpoint::Lease> {
    let native = agentchat_hosts::claude::process::ProcessIdentity::capture(std::process::id())?;
    endpoint::bind(path, scope, &native, None).await
}

fn private_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn identity(path: &Path) -> (u64, u64) {
    let m = fs::symlink_metadata(path).unwrap();
    (m.dev(), m.ino())
}
struct OwnedDirectory(PathBuf, (u64, u64));
impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        // All entries here were authored by this disposable test. The product
        // must never use this recursive fixture-only cleanup strategy.
        if self.0.exists() && identity(&self.0) == self.1 {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[tokio::test]
async fn dropping_a_gateway_retains_foreign_children_and_its_recovery_witness() {
    let root = private_root();
    let owner = Uuid::new_v4();
    let lease = bind(root.path(), owner).await.unwrap();
    let published = records::endpoint(root.path(), owner);
    let value: Endpoint = records::read(&published).unwrap();
    assert_eq!(
        lease.listener.local_addr().unwrap().as_pathname(),
        Some(value.socket.as_path())
    );
    let directory = value.socket.parent().unwrap().to_owned();
    let _owned = OwnedDirectory(directory.clone(), identity(&directory));
    let foreign = directory.join("foreign-note");
    fs::write(&foreign, b"preserve this unrelated note").unwrap();
    let id = identity(&foreign);
    let before = fs::read(&published).unwrap();
    drop(lease);
    assert!(
        foreign.exists(),
        "Gateway TempDir recursively removed a foreign child"
    );
    assert_eq!(identity(&foreign), id);
    assert_eq!(fs::read(foreign).unwrap(), b"preserve this unrelated note");
    assert!(!value.socket.exists());
    assert!(directory.exists());
    assert_eq!(fs::read(published).unwrap(), before);
}

#[tokio::test]
async fn dropping_a_gateway_never_removes_a_replacement_socket_parent() {
    let root = private_root();
    let owner = Uuid::new_v4();
    let lease = bind(root.path(), owner).await.unwrap();
    let published = records::endpoint(root.path(), owner);
    let record_id = identity(&published);
    let before = fs::read(&published).unwrap();
    let value: Endpoint = records::read(&published).unwrap();
    assert_eq!(
        lease.listener.local_addr().unwrap().as_pathname(),
        Some(value.socket.as_path())
    );
    let directory = value.socket.parent().unwrap().to_owned();
    let socket_id = identity(&value.socket);
    // Rename must stay on the socket filesystem, even when the isolated test
    // TMPDIR is mounted separately from /tmp on Linux.
    let holder = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(directory.parent().unwrap())
        .unwrap();
    let moved = holder.path().join("original-sockets");
    fs::rename(&directory, &moved).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let _owned = OwnedDirectory(directory.clone(), identity(&directory));
    fs::rename(moved.join("control.sock"), &value.socket).unwrap();
    let foreign = directory.join("foreign-note");
    fs::write(&foreign, b"foreign replacement parent").unwrap();
    let id = identity(&foreign);
    drop(lease);
    assert!(
        value.socket.exists(),
        "Gateway removed the original socket from a substituted parent"
    );
    assert_eq!(identity(&value.socket), socket_id);
    assert_eq!(identity(&foreign), id);
    assert_eq!(fs::read(foreign).unwrap(), b"foreign replacement parent");
    assert_eq!(identity(&published), record_id);
    assert_eq!(fs::read(published).unwrap(), before);
    assert!(moved.exists());
}

#[tokio::test]
async fn a_replaced_live_control_socket_keeps_its_endpoint_and_parent() {
    let root = private_root();
    let owner = Uuid::new_v4();
    let lease = bind(root.path(), owner).await.unwrap();
    let published = records::endpoint(root.path(), owner);
    let before = fs::read(&published).unwrap();
    let value: Endpoint = records::read(&published).unwrap();
    let directory = value.socket.parent().unwrap().to_owned();
    let _owned = OwnedDirectory(directory.clone(), identity(&directory));
    fs::remove_file(&value.socket).unwrap();
    let foreign = tokio::net::UnixListener::bind(&value.socket).unwrap();
    fs::set_permissions(&value.socket, fs::Permissions::from_mode(0o600)).unwrap();
    let id = identity(&value.socket);
    drop(lease);
    assert_eq!(identity(&value.socket), id);
    assert_eq!(fs::read(published).unwrap(), before);
    let _peer = tokio::net::UnixStream::connect(value.socket).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), foreign.accept())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn same_inode_endpoint_edits_are_kept_while_original_socket_cleanup_finishes() {
    let root = private_root();
    let owner = Uuid::new_v4();
    let lease = bind(root.path(), owner).await.unwrap();
    let path = records::endpoint(root.path(), owner);
    let value: Endpoint = records::read(&path).unwrap();
    let id = identity(&path);
    let bytes = b"foreign content on the original endpoint inode";
    fs::write(&path, bytes).unwrap();
    drop(lease);
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(!value.socket.parent().unwrap().exists());
}

#[tokio::test]
async fn endpoint_hardlink_aliases_preserve_both_names_and_bytes() {
    let root = private_root();
    let owner = Uuid::new_v4();
    let lease = bind(root.path(), owner).await.unwrap();
    let path = records::endpoint(root.path(), owner);
    let value: Endpoint = records::read(&path).unwrap();
    let id = identity(&path);
    let bytes = fs::read(&path).unwrap();
    let alias = root.path().join("alias");
    fs::hard_link(&path, &alias).unwrap();
    drop(lease);
    for path in [path, alias] {
        assert_eq!(identity(&path), id);
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert!(!value.socket.parent().unwrap().exists());
}

#[tokio::test]
async fn an_original_endpoint_moved_to_a_new_root_is_not_removed() {
    let root = private_root();
    let holder = private_root();
    let owner = Uuid::new_v4();
    let lease = bind(root.path(), owner).await.unwrap();
    let path = records::endpoint(root.path(), owner);
    let value: Endpoint = records::read(&path).unwrap();
    let id = identity(&path);
    let bytes = fs::read(&path).unwrap();
    let moved = holder.path().join("original-root");
    fs::rename(root.path(), &moved).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(root.path())
        .unwrap();
    fs::rename(moved.join(path.file_name().unwrap()), &path).unwrap();
    drop(lease);
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(!value.socket.parent().unwrap().exists());
}

#[tokio::test]
async fn normal_close_removes_only_its_publication_and_keeps_the_original_lock() {
    let root = private_root();
    let owner = Uuid::new_v4();
    let lease = bind(root.path(), owner).await.unwrap();
    let path = records::endpoint(root.path(), owner);
    let value: Endpoint = records::read(&path).unwrap();
    let lock = root.path().join(format!("gateway-{owner}.lock"));
    let id = identity(&lock);
    let nonce = lease.nonce;
    drop(lease);
    assert!(!path.exists());
    assert!(!value.socket.parent().unwrap().exists());
    assert_eq!(identity(&lock), id);
    assert!(fs::read(&lock).unwrap().is_empty());
    let next = bind(root.path(), owner).await.unwrap();
    assert_ne!(next.nonce, nonce);
    drop(next);
    assert_eq!(identity(&lock), id);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn unsafe_existing_lock_inodes_are_never_truncated_or_replaced() {
    for kind in ["contents", "alias", "symlink"] {
        let root = private_root();
        let owner = Uuid::new_v4();
        let path = root.path().join(format!("gateway-{owner}.lock"));
        let saved = root.path().join("saved");
        fs::write(&saved, b"foreign lock").unwrap();
        fs::set_permissions(&saved, fs::Permissions::from_mode(0o600)).unwrap();
        match kind {
            "contents" => fs::copy(&saved, &path).map(|_| ()).unwrap(),
            "alias" => fs::hard_link(&saved, &path).unwrap(),
            _ => std::os::unix::fs::symlink(&saved, &path).unwrap(),
        };
        let id = identity(&path);
        assert!(bind(root.path(), owner).await.is_err());
        assert_eq!(identity(&path), id);
        assert_eq!(fs::read(path).unwrap(), b"foreign lock");
        assert_eq!(fs::read(saved).unwrap(), b"foreign lock");
        assert!(!records::endpoint(root.path(), owner).exists());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_physical_gateway_creators_keep_one_original_lock_and_one_winner() {
    let root = private_root();
    let owner = Uuid::new_v4();
    let mut jobs = Vec::new();
    for _ in 0..16 {
        let path = root.path().to_owned();
        jobs.push(tokio::spawn(async move { bind(&path, owner).await }));
    }
    let mut winners = Vec::new();
    for job in jobs {
        if let Ok(lease) = job.await.unwrap() {
            winners.push(lease);
        }
    }
    assert_eq!(winners.len(), 1);
    let value: Endpoint = records::read(&records::endpoint(root.path(), owner)).unwrap();
    assert_eq!(value.nonce, winners[0].nonce);
    drop(winners);
    assert!(!value.socket.parent().unwrap().exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}
