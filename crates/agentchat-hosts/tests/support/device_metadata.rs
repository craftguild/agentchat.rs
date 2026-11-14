//! Simulate an original publication made before filesystem device renumbering.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub(crate) fn rewrite(path: &Path, change: impl FnOnce(&mut Value)) {
    let mut value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    change(&mut value);
    value["_agentchat_metadata"]["checksum"] = serde_json::to_value([0u8; 32]).unwrap();
    let digest: [u8; 32] = Sha256::new()
        .chain_update(b"agentchat/native-metadata/v1\0")
        .chain_update(serde_json::to_vec(&value).unwrap())
        .finalize()
        .into();
    value["_agentchat_metadata"]["checksum"] = json!(digest);
    // Keep the original inode, as a remount does. This constructs a valid old
    // publication; changing a stored number without resealing remains invalid.
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

pub(crate) fn renumber(root: &Path) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let kind = fs::symlink_metadata(&path).unwrap().file_type();
        if kind.is_dir() {
            renumber(&path);
        } else if kind.is_file()
            && let Ok(mut value) = serde_json::from_slice::<Value>(&fs::read(&path).unwrap())
            && value.get("_agentchat_metadata").is_some()
        {
            for field in ["parent", "file"] {
                let old = value["_agentchat_metadata"][field][0].as_u64().unwrap();
                value["_agentchat_metadata"][field][0] = json!(old ^ 1);
            }
            rewrite(&path, |record| *record = value);
        }
    }
}
