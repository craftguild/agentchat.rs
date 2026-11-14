//! Destructive recovery requires original publication proof and positive death.
use super::{
    files::same_user,
    identity::{Directory, Snapshot},
    ownership::Ownership,
    protocol::{self, Endpoint, Receipt, Retire},
    unavailable,
};
use crate::claude::process::{BirthState, ProcessIdentity};
use agentchat_core::Result;
use std::{ffi::OsStr, path::Path, time::Duration};
use tokio::net::UnixStream;

fn owner_permits(
    previous: &ProcessIdentity,
    current: &ProcessIdentity,
    observe: &impl Fn(&ProcessIdentity) -> BirthState,
) -> bool {
    previous == current || observe(previous) == BirthState::Gone
}
fn current(root: &Directory, name: &OsStr, original: &Snapshot) -> Result<bool> {
    match root.read(name)? {
        None => Ok(false),
        Some(now) if &now == original => Ok(true),
        _ => Err(unavailable()),
    }
}
fn original_directory(old: &Endpoint, proof: &Ownership) -> Result<Option<Directory>> {
    let path = old.socket.parent().ok_or_else(unavailable)?;
    if Directory::absent(path)? {
        return Ok(None);
    }
    let directory = Directory::open(path)?;
    if directory.id != proof.directory || directory.parent_id()? != proof.parent {
        return Err(unavailable());
    }
    for (name, original) in [
        ("request.sock", &proof.request),
        ("reply.sock", &proof.reply),
    ] {
        if directory
            .socket(name.as_ref())?
            .is_some_and(|id| &id != original)
        {
            return Err(unavailable());
        }
    }
    Ok(Some(directory))
}
async fn inactive(path: &Path) -> Result<()> {
    match UnixStream::connect(path).await {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(())
        }
        _ => Err(unavailable()),
    }
}
pub(super) async fn retire(path: &Path, observer: &str, owner: &ProcessIdentity) -> Result<()> {
    retire_with(path, observer, owner, ProcessIdentity::birth_state).await
}
async fn retire_with(
    path: &Path,
    observer: &str,
    owner: &ProcessIdentity,
    observe: impl Fn(&ProcessIdentity) -> BirthState,
) -> Result<()> {
    let root = Directory::open(path.parent().ok_or_else(unavailable)?)?;
    let name = path.file_name().ok_or_else(unavailable)?;
    let Some(snapshot) = root.read(name)? else {
        return Ok(());
    };
    let old: Endpoint = serde_json::from_slice(&snapshot.bytes).map_err(|_| unavailable())?;
    let previous = old.owner.as_ref().ok_or_else(unavailable)?;
    if old.observer != observer || !owner_permits(previous, owner, &observe) {
        return Err(unavailable());
    }
    // Legacy records remain readable for observation, but carry no original
    // parent/socket/relay-birth proof and cannot authorize destructive recovery.
    let proof = old.ownership.as_ref().ok_or_else(unavailable)?;
    proof.validate(&old, &root, snapshot.id)?;
    let directory = original_directory(&old, proof)?;
    let token = old.retire_token.ok_or_else(unavailable)?;
    match UnixStream::connect(&old.socket).await {
        Ok(mut stream) => {
            if !proof.relay.is_continuing()
                || !same_user(&stream)
                || !current(&root, name, &snapshot)?
            {
                return Err(unavailable());
            }
            protocol::write(&mut stream, &Retire { retire: token }).await?;
            let receipt: Receipt = protocol::read(&mut stream, 1024).await?;
            if !receipt.accepted || receipt.nonce != token {
                return Err(unavailable());
            }
            // A negotiated predecessor releases its own provenance. Never
            // unlink for it or overwrite a concurrent successor.
            for _ in 0..100 {
                if !current(&root, name, &snapshot)? {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(unavailable())
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            if observe(&proof.relay) != BirthState::Gone
                || !owner_permits(previous, owner, &observe)
                || !current(&root, name, &snapshot)?
            {
                return Err(unavailable());
            }
            if let Some(directory) = directory {
                // Probe both original sockets before deleting either. A child
                // may retain a listener after the relay's own birth is gone.
                inactive(&directory.path.join("request.sock")).await?;
                inactive(&directory.path.join("reply.sock")).await?;
                for (socket, id) in [
                    ("request.sock", &proof.request),
                    ("reply.sock", &proof.reply),
                ] {
                    if !current(&root, name, &snapshot)? {
                        return Ok(());
                    }
                    directory.remove_socket(socket.as_ref(), id)?;
                    #[cfg(test)]
                    tests::cut(socket, &root.path);
                }
                if !current(&root, name, &snapshot)? {
                    return Ok(());
                }
                directory.remove_empty(proof.parent)?;
                #[cfg(test)]
                tests::cut("directory", &root.path);
            }
            if current(&root, name, &snapshot)? {
                root.remove_record(name, &snapshot)?;
                #[cfg(test)]
                tests::cut("endpoint", &root.path);
            }
            Ok(())
        }
        Err(_) => Err(unavailable()),
    }
}

#[cfg(test)]
#[path = "../../../../tests/unit/native_relay_recovery.rs"]
mod tests;
