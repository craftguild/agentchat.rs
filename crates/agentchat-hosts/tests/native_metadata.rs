//! Private metadata, concurrent immutable publication and observation-only lookup.
use agentchat_hosts::claude::{
    metadata, namespace::OwnerNamespace, observer::NativeProfile, process::ProcessIdentity,
};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
#[path = "support/device_metadata.rs"]
mod device_metadata;

#[test]
fn device_renumbering_preserves_original_metadata_read_update_and_removal() {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let path = root.path().join("record.json");
    metadata::create(&path, &serde_json::json!({"owner": 1})).unwrap();
    let inode = path.metadata().unwrap().ino();
    device_metadata::renumber(root.path());
    let original = std::fs::read(&path).unwrap();
    assert_eq!(
        metadata::read::<serde_json::Value>(&path).unwrap(),
        serde_json::json!({"owner": 1})
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(path.metadata().unwrap().ino(), inode);
    metadata::write(&path, &serde_json::json!({"owner": 2})).unwrap();
    device_metadata::renumber(root.path());
    assert!(metadata::remove_if(&path, &serde_json::json!({"owner": 2})).unwrap());
    assert!(!path.exists());
}

#[test]
fn metadata_refuses_aliases_and_keeps_changed_values() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("record.json");
    metadata::create(&path, &serde_json::json!({"owner":1})).unwrap();
    assert!(metadata::create(&path, &serde_json::json!({"owner":2})).is_err());
    assert!(!metadata::remove_if(&path, &serde_json::json!({"owner":2})).unwrap());
    assert_eq!(
        metadata::read::<serde_json::Value>(&path).unwrap(),
        serde_json::json!({"owner":1})
    );
    let alias = root.path().join("hardlink.json");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(metadata::read::<serde_json::Value>(&path).is_err());
    assert!(metadata::write(&alias, &0).is_err());
    std::fs::remove_file(&alias).unwrap();
    let link = root.path().join("symlink.json");
    symlink(&path, &link).unwrap();
    assert!(metadata::read::<serde_json::Value>(&link).is_err());
    assert!(metadata::write(&link, &0).is_err());
    assert!(metadata::write(&path, &"x".repeat(65536)).is_err());
    assert_eq!(
        metadata::read::<serde_json::Value>(&path).unwrap(),
        serde_json::json!({"owner":1})
    );
    assert!(metadata::remove_if(&path, &serde_json::json!({"owner":1})).unwrap());
    assert!(!path.exists());
}
#[tokio::test]
async fn simultaneous_profile_publish_is_idempotent_and_missing_headers_never_reset_floors() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let ns = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
    let profile = NativeProfile {
        executable: owner.executable(),
        version: "2.1.301".into(),
    };
    let state = ns.directory().join("owner-state.json");
    let before = std::fs::read(&state).unwrap();
    let mut tasks = vec![];
    for _ in 0..16 {
        let n = ns.clone();
        let p = profile.clone();
        tasks.push(tokio::spawn(async move { n.publish_profile(p).await }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(std::fs::read(&state).unwrap(), before);
    let file = ns.directory().join("native-profile.json");
    let before = std::fs::read(&file).unwrap();
    let mut foreign = profile;
    foreign.version = "unvalidated".into();
    assert!(ns.publish_profile(foreign).await.is_err());
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert!(
        OwnerNamespace::existing(&home, owner.clone())
            .await
            .unwrap()
            .read_current()
            .await
            .unwrap()
            .is_none()
    );
    let lock = ns.directory().join("owner-state.lock");
    let inode = std::fs::metadata(&lock).unwrap().ino();
    std::fs::remove_file(&state).unwrap();
    assert!(OwnerNamespace::existing(&home, owner).await.is_err());
    assert!(!state.exists());
    assert_eq!(std::fs::metadata(&lock).unwrap().ino(), inode);
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert_eq!(
        std::fs::metadata(ns.directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn competing_immutable_publications_have_one_winner_and_no_staging_links() {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let path = root.path().join("record.json");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
    let mut threads = vec![];
    for value in 0..16 {
        let p = path.clone();
        let ready = barrier.clone();
        threads.push(std::thread::spawn(move || {
            ready.wait();
            (value, metadata::create(&p, &value).is_ok())
        }));
    }
    let winners: Vec<_> = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(|(_, won)| *won)
        .map(|(v, _)| v)
        .collect();
    assert_eq!(winners.len(), 1);
    assert_eq!(metadata::read::<u32>(&path).unwrap(), winners[0]);
    assert_eq!(std::fs::metadata(&path).unwrap().nlink(), 1);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
