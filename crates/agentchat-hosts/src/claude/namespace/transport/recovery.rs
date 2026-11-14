//! Finite synchronous recovery shared by rebind and dead-owner quarantine GC.
use super::TransportEndpoint;
use crate::claude::{
    metadata::{
        self,
        directory::{Directory, Snapshot},
    },
    process::{BirthState, ProcessIdentity},
};
use agentchat_core::Result;
use std::{ffi::OsStr, path::Path};
use uuid::Uuid;

pub(in crate::claude::namespace) enum Removed {
    Socket,
    Directory,
    Endpoint,
}
fn unavailable() -> agentchat_core::Error {
    agentchat_core::Error::new(
        agentchat_core::ErrorCode::SessionRequired,
        "The original physical gateway cannot be safely recovered.",
    )
}
fn inactive(path: &Path) -> Result<()> {
    crate::staging::inactive::socket(path)
}
pub(in crate::claude::namespace) fn recover_managed(
    home: &Path,
    root: &Directory,
    scope: Uuid,
    native: &ProcessIdentity,
    lock: &Snapshot,
    removed: &mut impl FnMut(Removed),
) -> Result<()> {
    recover_checked(
        root,
        scope,
        native,
        lock,
        removed,
        ProcessIdentity::birth_state,
        Some(home),
    )
}
pub(in crate::claude::namespace) fn recover(
    root: &Directory,
    scope: Uuid,
    native: &ProcessIdentity,
    lock: &Snapshot,
    removed: &mut impl FnMut(Removed),
) -> Result<()> {
    recover_with(
        root,
        scope,
        native,
        lock,
        removed,
        ProcessIdentity::birth_state,
    )
}
fn recover_with(
    root: &Directory,
    scope: Uuid,
    native: &ProcessIdentity,
    lock: &Snapshot,
    removed: &mut impl FnMut(Removed),
    observe: impl Fn(&ProcessIdentity) -> BirthState,
) -> Result<()> {
    recover_checked(root, scope, native, lock, removed, observe, None)
}
fn recover_checked(
    root: &Directory,
    scope: Uuid,
    native: &ProcessIdentity,
    lock: &Snapshot,
    removed: &mut impl FnMut(Removed),
    observe: impl Fn(&ProcessIdentity) -> BirthState,
    home: Option<&Path>,
) -> Result<()> {
    let name = format!("gateway-{scope}.json");
    let name = OsStr::new(&name);
    let Some(snapshot) = root.read(name)? else {
        return Ok(());
    };
    let old: TransportEndpoint = metadata::decode_proved(root, name, &snapshot)?;
    let proof = old.ownership.as_ref().ok_or_else(unavailable)?;
    if !lock.bytes.is_empty()
        || proof.version != 1
        || proof.scope != scope
        || proof.lock != lock.id
        || old.nonce.is_nil()
        || !old.socket.is_absolute()
        || old.socket.file_name() != Some(OsStr::new("control.sock"))
    {
        return Err(unavailable());
    }
    let lock_name = format!("gateway-{scope}.lock");
    let current = || -> Result<()> {
        if observe(&proof.helper) != BirthState::Gone
            || proof.native != *native && observe(&proof.native) != BirthState::Gone
            || !root.matches(OsStr::new(&lock_name), lock)?
            || !root.matches(name, &snapshot)?
        {
            return Err(unavailable());
        }
        Ok(())
    };
    current()?;
    let path = old.socket.parent().ok_or_else(unavailable)?;
    // Even an absent child needs its original parent. A substituted ancestor is
    // not evidence that the previous collector removed our original directory.
    if Directory::parent_of(path)? != proof.parent {
        return Err(unavailable());
    }
    let _journal = if let Some(home) = home {
        crate::staging::allocation::published_cleanup(
            home,
            path,
            root,
            (name, &snapshot),
            proof.directory,
            &[("control.sock", &proof.socket)],
            &proof.helper,
        )?
    } else {
        None
    };
    if !Directory::absent(path)? {
        let directory = Directory::open(path)?;
        if directory.id != proof.directory
            || directory
                .socket("control.sock".as_ref())?
                .is_some_and(|id| id != proof.socket)
        {
            return Err(unavailable());
        }
        inactive(&old.socket)?;
        current()?;
        if directory.remove_socket("control.sock".as_ref(), &proof.socket)? {
            removed(Removed::Socket);
            #[cfg(test)]
            tests::cut("socket", &root.path);
        }
        current()?;
        directory.remove_empty(proof.parent)?;
        removed(Removed::Directory);
        #[cfg(test)]
        tests::cut("directory", &root.path);
    }
    current()?;
    if root.remove_record(name, &snapshot)? {
        removed(Removed::Endpoint);
        #[cfg(test)]
        tests::cut("endpoint", &root.path);
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../../tests/unit/native_gateway_recovery.rs"]
mod tests;
