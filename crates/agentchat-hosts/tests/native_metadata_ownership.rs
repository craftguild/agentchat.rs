//! Actual filesystem mutations must retain original parents and full snapshots.
use agentchat_hosts::claude::metadata;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

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
struct Replaced {
    _root: tempfile::TempDir,
    _holder: tempfile::TempDir,
    path: PathBuf,
    bytes: Vec<u8>,
    id: (u64, u64),
}
impl Replaced {
    fn new() -> Self {
        let root = private_root();
        let holder = private_root();
        let path = root.path().join("owned.json");
        metadata::create(&path, &json!({"owner":1})).unwrap();
        let id = identity(&path);
        let bytes = fs::read(&path).unwrap();
        let moved = holder.path().join("original-parent");
        fs::rename(root.path(), &moved).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.path())
            .unwrap();
        fs::rename(moved.join("owned.json"), &path).unwrap();
        assert_eq!(identity(&path), id);
        Self {
            _root: root,
            _holder: holder,
            path,
            bytes,
            id,
        }
    }
    fn preserved(&self) {
        assert_eq!(identity(&self.path), self.id);
        assert_eq!(fs::read(&self.path).unwrap(), self.bytes);
    }
}

#[test]
fn original_record_moved_into_a_replacement_parent_cannot_authorize_removal() {
    let f = Replaced::new();
    let result = metadata::remove_if(&f.path, &json!({"owner":1}));
    assert!(
        !matches!(result, Ok(true)),
        "Removed a record through a substituted original parent: {result:?}"
    );
    f.preserved();
}

#[test]
fn original_record_moved_into_a_replacement_parent_cannot_authorize_update() {
    let f = Replaced::new();
    let result = metadata::write(&f.path, &json!({"owner":2}));
    assert!(
        result.is_err(),
        "Replaced a record through an unproved parent"
    );
    f.preserved();
}

const FOREIGN: &[u8] = b"{\"owner\":999,\"foreign\":true}";
struct ConcurrentEdit<'a>(&'a Path);
impl Serialize for ConcurrentEdit<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Serialization is a real public API callback. Join the foreign writer
        // before returning so the old same-inode race is deterministic.
        let path = self.0.to_owned();
        std::thread::spawn(move || fs::write(path, FOREIGN).unwrap())
            .join()
            .unwrap();
        json!({"owner":2}).serialize(serializer)
    }
}
#[test]
fn a_same_inode_foreign_edit_during_serialization_is_not_overwritten() {
    let root = private_root();
    let path = root.path().join("owned.json");
    metadata::create(&path, &json!({"owner":1})).unwrap();
    let id = identity(&path);
    let result = metadata::write(&path, &ConcurrentEdit(&path));
    assert!(
        result.is_err(),
        "Replaced foreign content written on the original inode"
    );
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(&path).unwrap(), FOREIGN);
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Compared {
    owner: u32,
    #[serde(skip)]
    edit: Option<PathBuf>,
}
impl PartialEq for Compared {
    fn eq(&self, other: &Self) -> bool {
        if let Some(path) = &other.edit {
            fs::write(path, FOREIGN).unwrap();
        }
        self.owner == other.owner
    }
}
#[test]
fn a_same_inode_foreign_edit_during_comparison_is_not_unlinked() {
    let root = private_root();
    let path = root.path().join("owned.json");
    metadata::create(&path, &json!({"owner":1})).unwrap();
    let id = identity(&path);
    let expected = Compared {
        owner: 1,
        edit: Some(path.clone()),
    };
    let result = metadata::remove_if(&path, &expected);
    assert!(
        !matches!(result, Ok(true)),
        "Deleted foreign content after comparison: {result:?}"
    );
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(&path).unwrap(), FOREIGN);
}

#[test]
fn copied_endpoint_bytes_do_not_authorize_the_new_inode() {
    let root = private_root();
    let path = root.path().join("owned.json");
    metadata::create(&path, &json!({"owner":1})).unwrap();
    let saved = root.path().join("saved.json");
    fs::rename(&path, &saved).unwrap();
    fs::copy(&saved, &path).unwrap();
    let id = identity(&path);
    let bytes = fs::read(&path).unwrap();
    assert_ne!(id, identity(&saved));
    let result = metadata::remove_if(&path, &json!({"owner":1}));
    assert!(
        !matches!(result, Ok(true)),
        "Adopted a copied record without original inode proof: {result:?}"
    );
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(saved).unwrap(), bytes);
}

#[test]
fn readable_legacy_content_does_not_itself_authorize_destructive_cleanup() {
    let root = private_root();
    let path = root.path().join("legacy.json");
    fs::write(&path, b"{\"owner\":1}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        metadata::read::<serde_json::Value>(&path).unwrap(),
        json!({"owner":1})
    );
    let before = fs::read(&path).unwrap();
    let id = identity(&path);
    let result = metadata::remove_if(&path, &json!({"owner":1}));
    assert!(
        !matches!(result, Ok(true)),
        "Legacy contents were adopted as deletion authority: {result:?}"
    );
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn native_object_fields_stay_flat_and_owned_updates_refresh_original_provenance() {
    let root = private_root();
    let path = root.path().join("wake.json");
    let value = json!({"context_id":"context", "token":"token", "accepted":true});
    metadata::create(&path, &value).unwrap();
    let first = identity(&path);
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    // Native JavaScript reads these body fields directly. The Rust read API
    // returns only the logical payload, never implementation provenance.
    for key in ["context_id", "token", "accepted"] {
        assert_eq!(raw[key], value[key]);
    }
    assert!(raw["_agentchat_metadata"].is_object());
    assert_eq!(metadata::read::<serde_json::Value>(&path).unwrap(), value);
    let next = json!({"context_id":"context", "token":"next", "accepted":false});
    metadata::write(&path, &next).unwrap();
    assert_ne!(identity(&path), first);
    assert_eq!(metadata::read::<serde_json::Value>(&path).unwrap(), next);
    assert!(metadata::remove_if(&path, &next).unwrap());
    assert!(!path.exists());
}

#[test]
fn unproved_legacy_data_is_not_silently_adopted_for_update() {
    let root = private_root();
    let path = root.path().join("legacy.json");
    let bytes = b"{\"owner\":1}";
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let id = identity(&path);
    assert!(metadata::write(&path, &json!({"owner":2})).is_err());
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn unsupported_publication_proof_is_preserved_without_downgrade() {
    let root = private_root();
    let path = root.path().join("record.json");
    metadata::create(&path, &json!({"owner":1})).unwrap();
    let id = identity(&path);
    let mut raw: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    raw["_agentchat_metadata"]["version"] = json!(2);
    let bytes = serde_json::to_vec(&raw).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(metadata::read::<serde_json::Value>(&path).is_err());
    assert!(metadata::write(&path, &json!({"owner":2})).is_err());
    assert!(metadata::remove_if(&path, &json!({"owner":1})).is_err());
    assert_eq!(identity(&path), id);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn changed_proof_bytes_cannot_hide_behind_equivalent_json_values() {
    for duplicate in [false, true] {
        let root = private_root();
        let path = root.path().join("record.json");
        metadata::create(&path, &json!({"owner":1})).unwrap();
        let id = identity(&path);
        let mut bytes = fs::read(&path).unwrap();
        if duplicate {
            assert_eq!(bytes.pop(), Some(b'}'));
            bytes.extend_from_slice(b",\"owner\":1}");
        } else {
            bytes.extend_from_slice(b"\n ");
        }
        fs::write(&path, &bytes).unwrap();
        assert!(metadata::read::<serde_json::Value>(&path).is_err());
        assert!(metadata::write(&path, &json!({"owner":2})).is_err());
        assert!(metadata::remove_if(&path, &json!({"owner":1})).is_err());
        assert_eq!(identity(&path), id);
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn legacy_typed_records_keep_the_original_duplicate_field_refusal() {
    let root = private_root();
    let path = root.path().join("legacy.json");
    fs::write(&path, b"{\"owner\":1,\"owner\":2}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(metadata::read::<Compared>(&path).is_err());
}
