//! Original ownership must precede destructive relay recovery; no host admission.
#[path = "relay_recovery/cases.rs"]
mod cases;
#[path = "relay_recovery/support.rs"]
mod support;
use agentchat_core::ErrorCode;
use agentchat_hosts::claude::{
    observer::relay,
    process::{BirthState, ProcessIdentity},
};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
};
use uuid::Uuid;

fn private_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("agentchat-relay-proof-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap()
}
fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
struct Inactive {
    _root: tempfile::TempDir,
    _sockets: tempfile::TempDir,
    endpoint: PathBuf,
    bytes: Vec<u8>,
    files: Vec<(PathBuf, (u64, u64))>,
}
impl Inactive {
    fn new(owner: &ProcessIdentity) -> Self {
        let root = private_root();
        let sockets = private_root();
        let request = sockets.path().join("request.sock");
        let reply = sockets.path().join("reply.sock");
        // Real inactive private sockets are not original ownership receipts.
        for path in [&request, &reply] {
            let listener = UnixListener::bind(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            drop(listener);
        }
        let endpoint = root.path().join("fresh-observer.json");
        let bytes = serde_json::to_vec(&json!({
            "observer":"observer", "socket":request, "pid":std::process::id(),
            "owner":owner, "retire_token":Uuid::new_v4()
        }))
        .unwrap();
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&endpoint)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        let files = [&endpoint, &request, &reply]
            .into_iter()
            .map(|path| (path.clone(), identity(path)))
            .collect();
        Self {
            _root: root,
            _sockets: sockets,
            endpoint,
            bytes,
            files,
        }
    }
    fn preserved(&self) -> bool {
        fs::read(&self.endpoint).ok().as_deref() == Some(self.bytes.as_slice())
            && self.files.iter().all(|(path, id)| {
                fs::symlink_metadata(path)
                    .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == *id)
            })
    }
    fn remaining(&self) -> Vec<bool> {
        self.files.iter().map(|(path, _)| path.exists()).collect()
    }
}

#[tokio::test]
async fn unequal_same_birth_image_facts_cannot_authorize_retirement() {
    let current = ProcessIdentity::capture(std::process::id()).unwrap();
    assert!(current.is_current());
    let mut value = serde_json::to_value(&current).unwrap();
    value["image"]["inode"] = json!(value["image"]["inode"].as_u64().unwrap() ^ 1);
    let previous: ProcessIdentity = serde_json::from_value(value).unwrap();
    assert_ne!(previous, current);
    assert!(!previous.is_current());
    // Real original kernel birth, with a synthetic full-image mismatch. This
    // exercises retention policy without claiming an actual native exec case.
    assert_eq!(previous.birth_state(), BirthState::Present);
    let fixture = Inactive::new(&previous);
    let result = relay::retire(fixture._root.path(), "observer", &current).await;
    assert!(
        fixture.preserved(),
        "A Present birth lost original metadata: remaining={:?}, result={:?}",
        fixture.remaining(),
        result
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::SessionRequired);
}

#[tokio::test]
async fn inactive_legacy_endpoint_cannot_claim_unproven_socket_ownership() {
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let fixture = Inactive::new(&owner);
    let result = relay::retire(fixture._root.path(), "observer", &owner).await;
    assert!(
        fixture.preserved(),
        "Private inactive paths were treated as original ownership: remaining={:?}, result={:?}",
        fixture.remaining(),
        result
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::SessionRequired);
}
