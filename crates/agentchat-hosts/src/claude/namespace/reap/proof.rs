//! Original seals admit quarantine; a certificate witnesses partial finalization.
use super::super::{OwnerNamespace, genesis, state::Header, storage, unavailable};
use super::{
    cut,
    files::{self, Stamp},
};
use crate::claude::process::{BirthState, ProcessIdentity};
use crate::lock::ScopedLock;
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Cleaning,
    Finalizing,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Certificate {
    version: u32,
    pub owner: ProcessIdentity,
    pub directory: (u64, u64),
    pub header: Stamp,
    pub marker: Stamp,
    pub lock: Stamp,
    pub gateway: Option<Stamp>,
    pub phase: Phase,
}
pub(super) fn key(ns: &OwnerNamespace) -> Result<String> {
    Ok(OwnerNamespace::locator(Path::new("/"), ns.owner.clone())?
        .directory
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(unavailable)?
        .to_owned())
}
pub(super) fn certificate_path(root: &Path, ns: &OwnerNamespace) -> Result<PathBuf> {
    Ok(root.join(format!(".gc-{}.json", key(ns)?)))
}
pub(super) fn namespace(home: &Path, certificate: &Certificate) -> Result<OwnerNamespace> {
    if certificate.version != 1
        || certificate.owner.birth_state() != BirthState::Gone
        || !certificate.header.in_directory(certificate.directory)
        || !certificate.marker.in_directory(certificate.directory)
        || !certificate.lock.in_directory(certificate.directory)
        || certificate
            .gateway
            .as_ref()
            .is_some_and(|s| !s.in_directory(certificate.directory))
    {
        return Err(unavailable());
    }
    let mut ns = OwnerNamespace::locator(home, certificate.owner.clone())?;
    ns.directory = ns
        .directory
        .parent()
        .ok_or_else(unavailable)?
        .join(format!(".retired-{}", key(&ns)?));
    Ok(ns)
}
pub(super) fn load(home: &Path, path: &Path) -> Result<(OwnerNamespace, Certificate)> {
    let (certificate, _): (Certificate, _) = files::record(path)?;
    let ns = namespace(home, &certificate)?;
    if certificate_path(path.parent().ok_or_else(unavailable)?, &ns)? != path {
        return Err(unavailable());
    }
    Ok((ns, certificate))
}
pub(super) fn locks(ns: &OwnerNamespace) -> Result<(ScopedLock, Option<ScopedLock>)> {
    let lock = genesis::open(ns, true, false)?;
    files::exclusive(&lock)?;
    let lock = ScopedLock::acquired(lock);
    let gateway = files::locked(&ns.directory.join(format!("gateway-{}.lock", ns.id)), false)?;
    Ok((lock, gateway))
}
fn original(ns: &OwnerNamespace, lock: &File) -> Result<Header> {
    let (header, _): (Header, _) = files::record(&ns.directory.join("owner-state.json"))?;
    let Some((record, sealed)) = genesis::record(ns, lock)? else {
        return Err(unavailable());
    };
    if !sealed || !genesis::marked(ns, &record)? {
        return Err(unavailable());
    }
    header.validate(
        ns,
        storage::identity(&lock.metadata().map_err(|_| unavailable())?),
    )?;
    if ns.owner.birth_state() != BirthState::Gone {
        return Err(unavailable());
    }
    Ok(header)
}
pub(super) fn verify(
    ns: &OwnerNamespace,
    certificate: &Certificate,
    lock: &File,
) -> Result<Header> {
    if files::directory(&ns.directory)? != certificate.directory
        || !certificate
            .header
            .matches(&ns.directory.join("owner-state.json"))?
        || !certificate
            .marker
            .matches(&ns.directory.join("owner-state.initialized.json"))?
        || !certificate
            .lock
            .matches(&ns.directory.join("owner-state.lock"))?
    {
        return Err(unavailable());
    }
    if let Some(stamp) = &certificate.gateway
        && !stamp.matches(&ns.directory.join(format!("gateway-{}.lock", ns.id)))?
    {
        return Err(unavailable());
    }
    original(ns, lock)
}
pub(super) fn admit(
    home: &Path,
    path: &Path,
    hook: &mut dyn FnMut(&str),
) -> Result<(OwnerNamespace, Certificate)> {
    let directory = files::directory(path)?;
    let (header, _): (Header, _) = files::record(&path.join("owner-state.json"))?;
    if header.owner.birth_state() != BirthState::Gone {
        return Err(unavailable());
    }
    let mut ns = OwnerNamespace::locator(home, header.owner)?;
    let root = ns.directory.parent().ok_or_else(unavailable)?.to_owned();
    let quarantine = root.join(format!(".retired-{}", key(&ns)?));
    if path != ns.directory && path != quarantine {
        return Err(unavailable());
    }
    ns.directory = path.to_owned();
    let (lock, _gateway) = locks(&ns)?;
    original(&ns, &lock)?;
    let mut certificate = Certificate {
        version: 1,
        owner: ns.owner.clone(),
        directory,
        header: Stamp::capture(&path.join("owner-state.json"))?,
        marker: Stamp::capture(&path.join("owner-state.initialized.json"))?,
        lock: Stamp::capture(&path.join("owner-state.lock"))?,
        gateway: None,
        phase: Phase::Cleaning,
    };
    if path != quarantine {
        if ns.owner.birth_state() != BirthState::Gone || files::directory(path)? != directory {
            return Err(unavailable());
        }
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            path,
            rustix::fs::CWD,
            &quarantine,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| unavailable())?;
        files::sync(&root)?;
        ns.directory = quarantine;
        cut(hook, "quarantined");
    }
    // No legitimate publisher can newly enter the quarantined path. If a
    // gateway opened its lock just before rename, wait for a later bounded pass.
    let gateway_path = ns.directory.join(format!("gateway-{}.lock", ns.id));
    let _late_gateway = if _gateway.is_none() {
        files::locked(&gateway_path, false)?
    } else {
        None
    };
    if files::exists(&gateway_path)? {
        certificate.gateway = Some(Stamp::capture(&gateway_path)?);
    }
    let target = certificate_path(&root, &ns)?;
    if files::exists(&target)? {
        if files::record::<Certificate>(&target)?.0 != certificate {
            return Err(unavailable());
        }
    } else {
        files::publish(
            home,
            &target,
            &certificate,
            None,
            Some(".collection.lock".as_ref()),
        )?;
    }
    cut(hook, "certified");
    Ok((ns, certificate))
}
