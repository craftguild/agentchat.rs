//! Actual killed publishers, exact retry prefixes and foreign-data preservation.
use super::support::{self, Fixture, NativeOwner};
use agentchat_hosts::claude::{
    observer::relay,
    process::{BirthState, ProcessIdentity},
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
};
use tokio::{io::AsyncWriteExt, net::UnixStream};

const RELAY: &str = "support::relay_process_child";
const NATIVE: &str = "support::native_owner_child";
fn preserved(snapshot: &[(std::path::PathBuf, (u64, u64))]) {
    for (path, id) in snapshot {
        assert_eq!(support::identity(path), *id, "{}", path.display());
    }
}

#[tokio::test]
async fn a_sigkilled_relay_is_recovered_while_its_native_owner_remains_alive() {
    let mut fixture = Fixture::new(RELAY).await;
    let owner = fixture.owner.clone();
    fixture.crash();
    assert!(owner.is_current());
    relay::retire(fixture.root.path(), "observer", &owner)
        .await
        .unwrap();
    assert!(!fixture.endpoint_path().exists());
    assert!(!fixture.directory.exists());
    fixture.restart(RELAY).await;
    // The replacement is an ordinary live relay and retires by its protocol,
    // rather than by process signals or an application identity reset.
    relay::retire(fixture.root.path(), "observer", &owner)
        .await
        .unwrap();
    fixture.finish().await;
    assert!(!fixture.endpoint_path().exists());
    assert!(owner.is_current());
}

#[tokio::test]
async fn dead_relay_cleanup_retries_each_missing_socket_and_directory_prefix() {
    for prefix in 0..=3 {
        let mut fixture = Fixture::new(RELAY).await;
        fixture.crash();
        if prefix >= 1 {
            fs::remove_file(fixture.directory.join("request.sock")).unwrap();
        }
        if prefix >= 2 {
            fs::remove_file(fixture.directory.join("reply.sock")).unwrap();
        }
        if prefix >= 3 {
            fs::remove_dir(&fixture.directory).unwrap();
        }
        relay::retire(fixture.root.path(), "observer", &fixture.owner)
            .await
            .unwrap();
        relay::retire(fixture.root.path(), "observer", &fixture.owner)
            .await
            .unwrap();
        assert!(!fixture.endpoint_path().exists());
        assert!(!fixture.directory.exists());
    }
}

#[tokio::test]
async fn actual_same_birth_exec_is_not_death_and_original_native_exit_permits_recovery() {
    let mut native = NativeOwner::new(NATIVE).await;
    let mut fixture = Fixture::with_owner(RELAY, native.token.clone()).await;
    fixture.crash();
    let before = fixture.snapshot();
    let current = native.exec().await;
    assert_ne!(current, native.token);
    assert!(!native.token.is_current());
    assert_eq!(native.token.birth_state(), BirthState::Present);
    assert!(
        relay::retire(fixture.root.path(), "observer", &current)
            .await
            .is_err()
    );
    preserved(&before);
    drop(native);
    assert_eq!(fixture.owner.birth_state(), BirthState::Gone);
    let current = ProcessIdentity::capture(std::process::id()).unwrap();
    relay::retire(fixture.root.path(), "observer", &current)
        .await
        .unwrap();
    assert!(!fixture.directory.exists());
    assert!(!fixture.endpoint_path().exists());
}

#[tokio::test]
async fn actual_normal_native_exit_allows_only_the_original_dead_relay_cleanup() {
    let mut native = NativeOwner::new(NATIVE).await;
    let mut fixture = Fixture::with_owner(RELAY, native.token.clone()).await;
    fixture.crash();
    native.exit();
    let current = ProcessIdentity::capture(std::process::id()).unwrap();
    relay::retire(fixture.root.path(), "observer", &current)
        .await
        .unwrap();
    assert!(!fixture.directory.exists());
    assert!(!fixture.endpoint_path().exists());
}

#[tokio::test]
async fn edited_endpoint_cannot_redirect_cleanup_to_foreign_private_sockets() {
    let mut fixture = Fixture::new(RELAY).await;
    fixture.crash();
    let foreign = support::private_root();
    for name in ["request.sock", "reply.sock"] {
        let path = foreign.path().join(name);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(listener);
    }
    let original = fixture.snapshot();
    let foreign_ids = [
        foreign.path().join("request.sock"),
        foreign.path().join("reply.sock"),
    ]
    .into_iter()
    .map(|p| {
        let id = support::identity(&p);
        (p, id)
    })
    .collect::<Vec<_>>();
    let mut value = fixture.endpoint.clone();
    value["socket"] = serde_json::json!(foreign.path().join("request.sock"));
    let bytes = serde_json::to_vec(&value).unwrap();
    fs::write(fixture.endpoint_path(), &bytes).unwrap();
    assert!(
        relay::retire(fixture.root.path(), "observer", &fixture.owner)
            .await
            .is_err()
    );
    assert_eq!(fs::read(fixture.endpoint_path()).unwrap(), bytes);
    preserved(&original);
    preserved(&foreign_ids);
}

#[tokio::test]
async fn aliased_and_copied_endpoint_records_preserve_all_original_socket_names() {
    for alias in [true, false] {
        let mut fixture = Fixture::new(RELAY).await;
        fixture.crash();
        let before = fixture.snapshot();
        let saved = fixture.root.path().join("saved");
        if alias {
            fs::hard_link(fixture.endpoint_path(), &saved).unwrap();
        } else {
            fs::rename(fixture.endpoint_path(), &saved).unwrap();
            fs::copy(&saved, fixture.endpoint_path()).unwrap();
        }
        let original = fs::read(&saved).unwrap();
        assert!(
            relay::retire(fixture.root.path(), "observer", &fixture.owner)
                .await
                .is_err()
        );
        assert_eq!(fs::read(fixture.endpoint_path()).unwrap(), original);
        assert_eq!(fs::read(&saved).unwrap(), original);
        preserved(&before[1..]);
    }
}

#[tokio::test]
async fn changed_socket_directory_preserves_even_the_original_socket_inodes() {
    let mut fixture = Fixture::new(RELAY).await;
    fixture.crash();
    let before = fixture.snapshot();
    let moved = fixture.root.path().join("original-sockets");
    fs::rename(&fixture.directory, &moved).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&fixture.directory)
        .unwrap();
    let replacement = fixture.directory.clone();
    fixture.remember_directory(&replacement);
    for name in ["request.sock", "reply.sock"] {
        fs::rename(moved.join(name), replacement.join(name)).unwrap();
    }
    preserved(&before);
    assert!(
        relay::retire(fixture.root.path(), "observer", &fixture.owner)
            .await
            .is_err()
    );
    preserved(&before);
    assert!(moved.exists());
}

#[tokio::test]
async fn replaced_or_live_socket_prevents_any_other_cleanup() {
    let mut fixture = Fixture::new(RELAY).await;
    fixture.crash();
    let request = fixture.directory.join("request.sock");
    fs::remove_file(&request).unwrap();
    let _foreign = std::os::unix::net::UnixListener::bind(&request).unwrap();
    fs::set_permissions(&request, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fixture.snapshot();
    assert!(
        relay::retire(fixture.root.path(), "observer", &fixture.owner)
            .await
            .is_err()
    );
    preserved(&before);
}

#[tokio::test]
async fn foreign_directory_entries_retain_the_endpoint_until_an_explicit_later_retry() {
    let mut fixture = Fixture::new(RELAY).await;
    fixture.crash();
    let foreign = fixture.directory.join("foreign");
    fs::write(&foreign, b"preserve me").unwrap();
    let before = fs::read(fixture.endpoint_path()).unwrap();
    assert!(
        relay::retire(fixture.root.path(), "observer", &fixture.owner)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&foreign).unwrap(), b"preserve me");
    assert_eq!(fs::read(fixture.endpoint_path()).unwrap(), before);
    fs::remove_file(&foreign).unwrap();
    relay::retire(fixture.root.path(), "observer", &fixture.owner)
        .await
        .unwrap();
    assert!(!fixture.directory.exists());
    assert!(!fixture.endpoint_path().exists());
}

#[tokio::test]
async fn ordinary_drop_keeps_same_inode_sockets_moved_into_a_replacement_parent() {
    let mut fixture = Fixture::new(RELAY).await;
    let before = fixture.snapshot();
    let moved = fixture.root.path().join("original-sockets");
    fs::rename(&fixture.directory, &moved).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&fixture.directory)
        .unwrap();
    let replacement = fixture.directory.clone();
    fixture.remember_directory(&replacement);
    for name in ["request.sock", "reply.sock"] {
        fs::rename(moved.join(name), replacement.join(name)).unwrap();
    }
    preserved(&before);
    fixture.stop().await;
    preserved(&before);
}

#[tokio::test]
async fn ordinary_drop_preserves_an_endpoint_changed_in_place() {
    let mut fixture = Fixture::new(RELAY).await;
    let id = support::identity(&fixture.endpoint_path());
    let bytes = b"foreign content on the original inode";
    fs::write(fixture.endpoint_path(), bytes).unwrap();
    fixture.stop().await;
    assert_eq!(support::identity(&fixture.endpoint_path()), id);
    assert_eq!(fs::read(fixture.endpoint_path()).unwrap(), bytes);
    assert!(!fixture.directory.exists());
}

#[tokio::test]
async fn ordinary_drop_keeps_an_original_endpoint_moved_to_a_replacement_root() {
    let mut fixture = Fixture::new(RELAY).await;
    let holder = support::private_root();
    let moved = holder.path().join("original-root");
    fs::rename(fixture.root.path(), &moved).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(fixture.root.path())
        .unwrap();
    fs::rename(moved.join("fresh-observer.json"), fixture.endpoint_path()).unwrap();
    let id = support::identity(&fixture.endpoint_path());
    let bytes = fs::read(fixture.endpoint_path()).unwrap();
    let mut stream = UnixStream::connect(moved.join("test-control.sock"))
        .await
        .unwrap();
    stream.write_all(b"s").await.unwrap();
    fixture.finish().await;
    assert_eq!(support::identity(&fixture.endpoint_path()), id);
    assert_eq!(fs::read(fixture.endpoint_path()).unwrap(), bytes);
    assert!(!fixture.directory.exists());
}
