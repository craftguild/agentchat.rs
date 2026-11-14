//! Kernel-backed locks retain exclusion until scope end, despite duplicate FDs.
use super::*;
use fs2::FileExt;
use std::fs::OpenOptions;

#[test]
fn a_duplicate_description_cannot_extend_a_finished_coordination_lock() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("coordination.lock");
    let first = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    first.try_lock_exclusive().unwrap();
    let guard = ScopedLock::acquired(first);
    let inherited = guard.try_clone().unwrap();
    let next = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert_eq!(
        next.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(guard);
    // A fork preserves the same open-file description as this duplicate. Its
    // incidental lifetime must not extend the parent's completed mutex scope.
    next.try_lock_exclusive().unwrap();
    let next = ScopedLock::acquired(next);
    drop(inherited);
    let foreign = File::open(&path).unwrap();
    assert_eq!(
        foreign.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(next);
    foreign.try_lock_exclusive().unwrap();
}

#[test]
fn a_shared_scope_releases_without_unlocking_a_later_description() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("shared.lock");
    let first = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    FileExt::try_lock_shared(&first).unwrap();
    let guard = ScopedLock::acquired(first);
    let inherited = guard.try_clone().unwrap();
    let next = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert_eq!(
        next.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(guard);
    next.try_lock_exclusive().unwrap();
    drop(inherited);
    let another = File::open(&path).unwrap();
    assert_eq!(
        another.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    FileExt::unlock(&next).unwrap();
    another.try_lock_exclusive().unwrap();
}
