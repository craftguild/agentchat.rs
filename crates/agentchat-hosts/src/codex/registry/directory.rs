//! Private root index and immutable serialization lock genesis.
use super::super::required;
use crate::claude::metadata::{
    self, Publisher,
    directory::{Directory, Snapshot},
};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::DirBuilderExt, path::Path};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootLock {
    version: u32,
    root: Uuid,
    nonce: Uuid,
}
pub(super) fn open(home: &Path, create: bool) -> Result<Option<Directory>> {
    if !create {
        match fs::symlink_metadata(home) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(required()),
            Ok(_) => {}
        }
    }
    let mut path = home.to_owned();
    metadata::private_directory(&path)?;
    for component in ["hosts", "codex", "roots"] {
        let parent = Directory::owned(&path)?;
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(required()),
                }
            }
            Err(_) => return Err(required()),
        }
        let current = Directory::owned(&path)?;
        if create {
            current.sync()?;
            parent.sync()?;
        }
    }
    Directory::owned(&path).map(Some)
}
pub(super) fn root_lock(
    home: &Path,
    directory: &Directory,
    root: Uuid,
    name: &str,
) -> Result<Snapshot> {
    if directory.read(name.as_ref())?.is_none() {
        let value = RootLock {
            version: 1,
            root,
            nonce: Uuid::new_v4(),
        };
        if let Err(error) = Publisher::new(home).create(&directory.path.join(name), &value)
            && directory.read(name.as_ref())?.is_none()
        {
            return Err(error);
        }
    }
    let snapshot = directory.read(name.as_ref())?.ok_or_else(required)?;
    let value: RootLock = metadata::decode_proved(directory, name.as_ref(), &snapshot)?;
    if value.version != 1 || value.root != root || value.nonce.is_nil() {
        return Err(required());
    }
    Ok(snapshot)
}
