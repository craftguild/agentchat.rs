//! Exact transport projections only; application history and cwd signals survive.
use super::super::{LifecycleOwner, OwnerNamespace, state::Header, unavailable};
use super::{
    ReapReport, cut, files,
    proof::{self, Certificate, Phase},
};
use crate::claude::{
    NativeLifecycle, NativeRegistration,
    observer::{NativeProfile, RegisteredOwner},
    routing,
};
use agentchat_core::Result;
use std::path::Path;

fn registration(ns: &OwnerNamespace, header: &Header, value: &RegisteredOwner) -> Result<()> {
    let r = &value.registration;
    if value.owner != ns.owner
        || r.owner_root != ns.id
        || r.observer != ns.observer
        || r.generation == 0
        || r.generation > header.generation
        || r.root.is_nil()
        || r.context.context_id.is_nil()
        || r.context.incarnation.is_nil()
        || r.context.project_id.is_nil()
    {
        return Err(unavailable());
    }
    NativeLifecycle::Project(r.clone()).validate()
}
pub(super) fn notices(
    home: &Path,
    ns: &OwnerNamespace,
    r: &NativeRegistration,
    directory: (u64, u64),
) -> Result<usize> {
    let count = super::super::projections::remove_notices_in(&ns.directory, r, directory)?;
    if files::directory(&ns.directory)? != directory {
        return Err(unavailable());
    }
    let removed = routing::remove(
        home,
        &RegisteredOwner {
            registration: r.clone(),
            owner: ns.owner.clone(),
        },
    )?;
    Ok(count + usize::from(removed))
}
pub(super) fn witnesses(ns: &OwnerNamespace, certificate: &Certificate, name: &str) -> bool {
    matches!(
        name,
        "owner-state.json" | "owner-state.lock" | "owner-state.initialized.json"
    ) || (certificate.gateway.is_some() && name == format!("gateway-{}.lock", ns.id))
}
pub(super) fn one(
    home: &Path,
    ns: &OwnerNamespace,
    header: &Header,
    certificate: &Certificate,
    path: &Path,
    report: &mut ReapReport,
) -> Result<usize> {
    if files::directory(&ns.directory)? != certificate.directory {
        return Err(unavailable());
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(unavailable)?;
    if witnesses(ns, certificate, name) {
        return Ok(0);
    }
    if name == "input-head.json" {
        let head = crate::claude::body::InputHead::read(&ns.directory)?.ok_or_else(unavailable)?;
        if head.native_generation() > header.generation {
            return Err(unavailable());
        }
        return Ok(usize::from(head.remove(&ns.directory)?));
    }
    if name.starts_with("input-") {
        // Prefix selects a candidate only. Original capsule provenance pins
        // the exact parent/inode/name/content; the collector already holds
        // positive original-owner loss and this namespace's lifecycle locks.
        // Read-only legacy inspection here is confined to this positively dead
        // owner's collector. Public input/load/entry still rejects old capsules.
        let input = crate::claude::body::InputFile::read_retained(path)?;
        if input.native_generation > header.generation {
            return Err(unavailable());
        }
        return Ok(usize::from(input.remove_retained(&ns.directory)?));
    }
    // Collect each original-proved body transport record independently. This
    // preserves the collector's per-page removal bound and supports any crash
    // ordering between receipt, payload and pending-pointer removal.
    if name.starts_with("entered-input-") {
        let entry = crate::claude::body::InputEntry::read(path)?;
        if entry.native_generation() > header.generation {
            return Err(unavailable());
        }
        return Ok(usize::from(entry.remove(&ns.directory)?));
    }
    if name.starts_with("accepted-input-") {
        let receipt = crate::claude::body::InputReceipt::read(path)?;
        if receipt.native_generation > header.generation {
            return Err(unavailable());
        }
        return Ok(usize::from(receipt.remove(&ns.directory)?));
    }
    if name.starts_with("pending-input-") {
        let pending = crate::claude::body::PendingInput::read(path)?;
        if pending.native_generation > header.generation {
            return Err(unavailable());
        }
        return Ok(usize::from(pending.remove(&ns.directory)?));
    }
    if name == format!("current-owner-{}.json", ns.id) {
        let (value, stamp): (LifecycleOwner, _) = files::small_record(path)?;
        if value.owner != ns.owner
            || header.current.as_ref().map(|c| &c.lifecycle) != Some(&value.registration)
        {
            return Err(unavailable());
        }
        let count = if let NativeLifecycle::Project(r) = &value.registration {
            notices(home, ns, r, certificate.directory)?
        } else {
            0
        };
        return Ok(count + usize::from(remove(&stamp, path, certificate.directory)?));
    }
    if name == format!("native-{}.json", ns.observer) || name.starts_with("context-") {
        let (value, stamp): (RegisteredOwner, _) = files::small_record(path)?;
        registration(ns, header, &value)?;
        if name.starts_with("context-")
            && name != format!("context-{}.json", value.registration.context.context_id)
        {
            return Err(unavailable());
        }
        let count = notices(home, ns, &value.registration, certificate.directory)?;
        return Ok(count + usize::from(remove(&stamp, path, certificate.directory)?));
    }
    if name == "native-profile.json" {
        let (value, stamp): (NativeProfile, _) = files::small_record(path)?;
        if value.executable != ns.owner.executable()
            || !crate::claude::module::candidate_version(&value.version)
        {
            return Err(unavailable());
        }
        return Ok(usize::from(remove(&stamp, path, certificate.directory)?));
    }
    if name == format!("gateway-{}.json", ns.id) {
        use super::super::transport::{self, Removed};
        let root = crate::claude::metadata::directory::Directory::owned(&ns.directory)?;
        if (root.id.0, root.id.1) != certificate.directory {
            return Err(unavailable());
        }
        let lock = root
            .read(format!("gateway-{}.lock", ns.id).as_ref())?
            .ok_or_else(unavailable)?;
        // proof::locks already holds this original lock. Account each completed
        // prefix even if a later foreign entry makes recovery preserve the rest.
        transport::recover_managed(
            home,
            &root,
            ns.id,
            &ns.owner,
            &lock,
            &mut |item| match item {
                Removed::Directory => report.directories_removed += 1,
                Removed::Socket | Removed::Endpoint => report.removed += 1,
            },
        )?;
        return Ok(0);
    }
    // Wake/submission files are removed through their retained registration or
    // Header.current, never by their filename prefix or an unrelated UUID.
    Err(unavailable())
}
fn remove(stamp: &files::Stamp, path: &Path, directory: (u64, u64)) -> Result<bool> {
    if !stamp.in_directory(directory) {
        return Err(unavailable());
    }
    stamp.remove(path)
}
pub(super) fn ready(ns: &OwnerNamespace, certificate: &Certificate) -> Result<bool> {
    // There are at most four witnesses. A fifth entry or any other name keeps
    // the complete proof/certificate, without recursively deleting foreign data.
    for entry in std::fs::read_dir(&ns.directory)
        .map_err(|_| unavailable())?
        .take(5)
    {
        let entry = entry.map_err(|_| unavailable())?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| witnesses(ns, certificate, name))
        {
            return Ok(false);
        }
    }
    Ok(true)
}
pub(super) fn finalize(
    home: &Path,
    ns: &OwnerNamespace,
    certificate: &Certificate,
    report: &mut ReapReport,
    hook: &mut dyn FnMut(&str),
) -> Result<()> {
    let root = ns.directory.parent().ok_or_else(unavailable)?;
    let path = proof::certificate_path(root, ns)?;
    let (mut current, stamp): (Certificate, _) = files::record(&path)?;
    if current != *certificate {
        return Err(unavailable());
    }
    if !files::exists(&ns.directory)? {
        report.removed += usize::from(stamp.remove(&path)?);
        return Ok(());
    }
    if files::directory(&ns.directory)? != current.directory
        || current.owner.birth_state() != crate::claude::process::BirthState::Gone
    {
        return Err(unavailable());
    }
    if current.phase == Phase::Cleaning {
        if !ready(ns, &current)? {
            return Err(unavailable());
        }
        current.phase = Phase::Finalizing;
        files::publish(
            home,
            &path,
            &current,
            Some(&stamp),
            Some(".collection.lock".as_ref()),
        )?;
        cut(hook, "finalizing");
    }
    let mut owned = vec![
        ("owner-state.initialized.json".to_owned(), &current.marker),
        ("owner-state.json".to_owned(), &current.header),
    ];
    if let Some(gateway) = &current.gateway {
        owned.push((format!("gateway-{}.lock", ns.id), gateway));
    }
    owned.push(("owner-state.lock".to_owned(), &current.lock));
    for (name, expected) in &owned {
        let file = ns.directory.join(name);
        if files::exists(&file)? && !expected.matches(&file)? {
            return Err(unavailable());
        }
    }
    for (name, expected) in owned {
        report.removed += usize::from(expected.remove(&ns.directory.join(&name))?);
        cut(hook, &name);
        if name.starts_with("gateway-") {
            cut(hook, "gateway_removed");
        }
    }
    if files::directory(&ns.directory)? != current.directory {
        return Err(unavailable());
    }
    if std::fs::read_dir(&ns.directory)
        .map_err(|_| unavailable())?
        .next()
        .is_some()
    {
        return Err(unavailable());
    }
    std::fs::remove_dir(&ns.directory).map_err(|_| unavailable())?;
    files::sync(root)?;
    report.directories_removed += 1;
    cut(hook, "directory_removed");
    let (actual, stamp): (Certificate, _) = files::record(&path)?;
    if actual != current {
        return Err(unavailable());
    }
    report.removed += usize::from(stamp.remove(&path)?);
    cut(hook, "certificate_removed");
    let _ = home;
    Ok(())
}
