//! Relevant UUID activity remains authoritative after the state tree disappears.

use agentchat_core::ErrorCode;
use agentchat_hosts::lifecycle::Lifecycle;
use std::{fs, os::unix::fs::symlink};
use uuid::Uuid;

#[test]
fn local_quiescence_ignores_other_projects_and_global_requires_all_owners_to_finish() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap().join("state");
    fs::create_dir(&root).unwrap();
    let lifecycle = Lifecycle::new(&root).unwrap();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let active = lifecycle.activity(first).unwrap();
    fs::remove_dir_all(&root).unwrap();
    fs::create_dir(&root).unwrap();
    let fence = lifecycle.exclusive().unwrap();
    assert_eq!(
        lifecycle.quiescent(&fence, Some(first)).err().unwrap().code,
        ErrorCode::Busy
    );
    assert!(
        lifecycle
            .quiescent(&fence, Some(second))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        lifecycle.quiescent(&fence, None).err().unwrap().code,
        ErrorCode::Busy
    );
    drop(active);
    assert_eq!(lifecycle.quiescent(&fence, Some(first)).unwrap().len(), 1);
    assert_eq!(lifecycle.quiescent(&fence, None).unwrap().len(), 1);
}

#[test]
fn uncertain_relevant_metadata_is_preserved_without_blocking_unrelated_local_scope() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap().join("state");
    let lifecycle = Lifecycle::new(&root).unwrap();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    drop(lifecycle.activity(first).unwrap());
    let record = lifecycle
        .directory()
        .join("active")
        .join(format!("{first}.lock"));
    fs::remove_file(&record).unwrap();
    let foreign = fixture.path().join("foreign");
    fs::write(&foreign, b"preserve foreign bytes").unwrap();
    symlink(&foreign, &record).unwrap();
    let fence = lifecycle.exclusive().unwrap();
    assert!(lifecycle.quiescent(&fence, Some(first)).is_err());
    assert!(lifecycle.quiescent(&fence, None).is_err());
    assert!(
        lifecycle
            .quiescent(&fence, Some(second))
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read(&foreign).unwrap(), b"preserve foreign bytes");
}

#[test]
fn quiescence_cannot_be_checked_under_another_roots_or_shared_fence() {
    let fixture = tempfile::tempdir().unwrap();
    let base = fixture.path().canonicalize().unwrap();
    let lifecycle = Lifecycle::new(&base.join("state")).unwrap();
    let other = Lifecycle::new(&base.join("other")).unwrap();
    let foreign = other.exclusive().unwrap();
    assert!(lifecycle.quiescent(&foreign, None).is_err());
    drop(foreign);
    let shared = lifecycle.shared().unwrap();
    assert!(lifecycle.quiescent(&shared, None).is_err());
}
