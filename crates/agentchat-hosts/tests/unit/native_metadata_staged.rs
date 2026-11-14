//! Original held parents/stages and complete byte snapshots, using real files.
use super::*;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
};
#[path = "published_lock_lifetime.rs"]
mod lock_lifetime;
#[path = "native_origin_publishers.rs"]
pub(super) mod publishers;
#[path = "stage_retention.rs"]
pub(super) mod retention;

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn identity(path: &std::path::Path) -> FileId {
    FileId::of(&fs::symlink_metadata(path).unwrap())
}

#[test]
fn full_256k_raw_header_publication_and_replacement_keep_the_original_bound() {
    let root = root();
    let parent = Directory::owned(root.path()).unwrap();
    let data = vec![b'x'; 262_144];
    let mut stage = Staged::new(&parent, ".test-").unwrap();
    let id = FileId::of(&stage.file.metadata().unwrap());
    stage.file.write_all(&data).unwrap();
    stage.publish("header".as_ref(), &data, None).unwrap();
    let prior = parent
        .read_bound("header".as_ref(), 262_144)
        .unwrap()
        .unwrap();
    assert_eq!(prior.id, id);
    assert_eq!(prior.bytes, data);
    let next = vec![b'y'; 262_144];
    let mut stage = Staged::new(&parent, ".test-").unwrap();
    stage.file.write_all(&next).unwrap();
    stage
        .publish("header".as_ref(), &next, Some(&prior))
        .unwrap();
    assert_eq!(
        parent
            .read_bound("header".as_ref(), 262_144)
            .unwrap()
            .unwrap()
            .bytes,
        next
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(fs::metadata(root.path().join("header")).unwrap().nlink(), 1);
}

#[test]
fn a_replaced_parent_cannot_redirect_stage_publication_or_cleanup() {
    let original = root();
    let holder = root();
    let parent = Directory::owned(original.path()).unwrap();
    let mut stage = Staged::new(&parent, ".test-").unwrap();
    let name = stage.name.clone();
    let data = b"original bytes";
    stage.file.write_all(data).unwrap();
    let moved = holder.path().join("original");
    fs::rename(original.path(), &moved).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(original.path())
        .unwrap();
    let foreign = original.path().join(&name);
    fs::write(&foreign, b"foreign bytes").unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    let id = identity(&foreign);
    assert!(stage.publish("published".as_ref(), data, None).is_err());
    assert_eq!(identity(&foreign), id);
    assert_eq!(fs::read(foreign).unwrap(), b"foreign bytes");
    assert_eq!(fs::read(moved.join(name)).unwrap(), data);
    assert!(!original.path().join("published").exists());
    assert!(!moved.join("published").exists());
}

#[test]
fn a_replaced_stage_keeps_both_the_original_and_foreign_inode() {
    let root = root();
    let parent = Directory::owned(root.path()).unwrap();
    let mut stage = Staged::new(&parent, ".test-").unwrap();
    let path = root.path().join(&stage.name);
    let data = b"original";
    stage.file.write_all(data).unwrap();
    let id = identity(&path);
    let saved = root.path().join("saved");
    fs::rename(&path, &saved).unwrap();
    fs::write(&path, b"foreign").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let foreign = identity(&path);
    assert_ne!(foreign, id);
    assert!(stage.publish("published".as_ref(), data, None).is_err());
    assert_eq!(identity(&saved), id);
    assert_eq!(fs::read(saved).unwrap(), data);
    assert_eq!(identity(&path), foreign);
    assert_eq!(fs::read(path).unwrap(), b"foreign");
}

#[test]
fn a_same_inode_foreign_stage_edit_cannot_be_published() {
    let root = root();
    let parent = Directory::owned(root.path()).unwrap();
    let mut stage = Staged::new(&parent, ".test-").unwrap();
    let path = root.path().join(&stage.name);
    let data = b"original";
    stage.file.write_all(data).unwrap();
    let id = identity(&path);
    fs::write(&path, b"foreign").unwrap();
    assert!(stage.publish("published".as_ref(), data, None).is_err());
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(path).unwrap(), b"foreign");
    assert!(!root.path().join("published").exists());
}

#[test]
fn typed_first_creator_collision_releases_only_the_complete_own_stage() {
    let root = root();
    let parent = Directory::owned(root.path()).unwrap();
    let path = root.path().join("published");
    fs::write(&path, b"foreign").unwrap();
    let id = identity(&path);
    let mut stage = Staged::new(&parent, ".test-").unwrap();
    let staged = root.path().join(&stage.name);
    stage.file.write_all(b"original").unwrap();
    let error = stage
        .publish("published".as_ref(), b"original", None)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(!staged.exists());
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(path).unwrap(), b"foreign");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn ordinary_drop_preserves_an_original_stage_changed_after_sealing() {
    let root = root();
    let parent = Directory::owned(root.path()).unwrap();
    let mut stage = Staged::new(&parent, ".test-").unwrap();
    let path = root.path().join(&stage.name);
    stage.file.write_all(b"original").unwrap();
    stage.seal(b"original").unwrap();
    let id = identity(&path);
    fs::write(&path, b"foreign after seal").unwrap();
    drop(stage);
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(path).unwrap(), b"foreign after seal");
}
