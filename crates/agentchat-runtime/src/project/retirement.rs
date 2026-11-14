//! Surviving request tombstones prevent removed setup receipts being replayed.

use super::{ProjectManager, fileio, journal};
use agentchat_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf};
use uuid::Uuid;

#[cfg(test)]
#[path = "../../tests/unit/removal_preflight.rs"]
mod tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retired {
    version: u32,
    request: Uuid,
    project: Uuid,
}

fn record_path(manager: &ProjectManager, kind: &str, request: Uuid) -> Result<PathBuf> {
    let lifecycle = manager.lifecycle()?;
    for directory in [
        lifecycle.directory().to_owned(),
        lifecycle.directory().join(kind),
    ] {
        match fs::symlink_metadata(directory) {
            Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                return Err(fileio::conflict(
                    "Retired request metadata has a foreign directory type.",
                ));
            }
            Ok(meta)
                if meta.uid() != rustix::process::geteuid().as_raw()
                    || meta.mode() & 0o077 != 0 =>
            {
                return Err(fileio::conflict(
                    "Request retirement directory is not private and owned.",
                ));
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(fileio::io(e)),
        }
    }
    Ok(lifecycle
        .directory()
        .join(kind)
        .join(format!("{request}.json")))
}

pub(super) fn check(manager: &ProjectManager, request: Uuid) -> Result<()> {
    if let Some(value) = read(manager, "retired", request)? {
        if value.version != 1 || value.request != request {
            return Err(fileio::conflict(
                "Invalid retired installation request record.",
            ));
        }
        let mut error = Error::new(
            ErrorCode::IdempotencyConflict,
            "This installation request was retired by uninstall. Use a fresh request ID for an explicit installation.",
        );
        error.details =
            serde_json::json!({"reason":"installation_request_retired","request_id":request});
        return Err(error);
    }
    Ok(())
}

/// The external index is retirement authority only, never installation health.
/// Record intent before reporting any successful project receipt, so deleting
/// the selected state root cannot erase the keys a later reset must retire.
pub(super) fn remember(manager: &ProjectManager, request: Uuid, project: Uuid) -> Result<()> {
    let path = record_path(manager, "setup-requests", request)?;
    if let Some(existing) = read(manager, "setup-requests", request)? {
        if existing.project != project {
            return Err(journal::idempotency_conflict());
        }
        return fileio::durable(&path);
    }
    fileio::write(
        &path,
        &Retired {
            version: 1,
            request,
            project,
        },
    )
}

/// Caller owns the initialization lock. A known historical key with lost state
/// cannot be reused to allocate another namespace, even before explicit reset.
pub(super) fn available(manager: &ProjectManager, request: Uuid) -> Result<()> {
    if read(manager, "setup-requests", request)?.is_some()
        && journal::completed(manager, request)?.is_none()
        && journal::pending(manager)?.is_none_or(|p| p.request.request.request_id != request)
    {
        let mut error = agentchat_core::Error::new(
            ErrorCode::IdempotencyConflict,
            "This historical setup request has lost its selected-state receipt. Uninstall can retire it; use a fresh request ID with an explicit recovery installation.",
        );
        error.details =
            serde_json::json!({"reason":"historical_setup_state_missing","request_id":request});
        return Err(error);
    }
    Ok(())
}

fn read(manager: &ProjectManager, kind: &str, request: Uuid) -> Result<Option<Retired>> {
    let path = record_path(manager, kind, request)?;
    match fs::symlink_metadata(&path) {
        Ok(meta)
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.nlink() != 1
                || meta.uid() != rustix::process::geteuid().as_raw()
                || meta.mode() & 0o077 != 0 =>
        {
            return Err(fileio::conflict(
                "External setup request metadata is not private and owned.",
            ));
        }
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(fileio::io(e)),
    }
    let value: Retired = fileio::read(&path)?
        .ok_or_else(|| fileio::conflict("External setup request disappeared."))?;
    if value.version != 1 || value.request != request || value.project.get_version_num() != 4 {
        return Err(fileio::conflict("Invalid external setup request identity."));
    }
    Ok(Some(value))
}

/// Call only while holding the exclusive lifecycle fence, before data removal.
/// Records are immutable and survive both local and global deletion targets.
pub(super) fn retire(manager: &ProjectManager, project: Option<Uuid>) -> Result<()> {
    let registry = manager.lifecycle()?.directory().join("setup-requests");
    match fs::read_dir(&registry) {
        Ok(entries) => {
            for (index, entry) in entries.enumerate() {
                if index >= 65_536 {
                    return Err(fileio::conflict(
                        "External setup request inventory exceeds its bound.",
                    ));
                }
                let entry = entry.map_err(fileio::io)?;
                let request = entry
                    .path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or_else(|| fileio::conflict("Unknown external setup request filename."))?;
                let record = read(manager, "setup-requests", request)?.ok_or_else(|| {
                    fileio::conflict("External setup request disappeared during retirement.")
                })?;
                retire_receipt(manager, request, record.project, project)?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(fileio::io(e)),
    }
    if let Some(pending) = journal::pending(manager)? {
        retire_receipt(
            manager,
            pending.request.request.request_id,
            pending.receipt.binding.identity.id,
            project,
        )?;
    }
    let directory = manager.home().join("init/requests");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(fileio::io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(fileio::io)?;
        let request = entry
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| fileio::conflict("Unknown file in installation receipt directory."))?;
        let receipt = journal::completed(manager, request)?.ok_or_else(|| {
            fileio::conflict("Installation receipt disappeared before retirement.")
        })?;
        let namespace = receipt.receipt.binding.identity.id;
        retire_receipt(manager, request, namespace, project)?;
    }
    Ok(())
}

fn retire_receipt(
    manager: &ProjectManager,
    request: Uuid,
    namespace: Uuid,
    selected: Option<Uuid>,
) -> Result<()> {
    if selected.is_some_and(|id| id != namespace) {
        return Ok(());
    }
    let target = record_path(manager, "retired", request)?;
    if let Some(existing) = read(manager, "retired", request)? {
        if existing.version != 1 || existing.request != request || existing.project != namespace {
            return Err(fileio::conflict(
                "Retired installation request differs from its receipt.",
            ));
        }
        fileio::durable(&target)
    } else {
        fileio::write(
            &target,
            &Retired {
                version: 1,
                request,
                project: namespace,
            },
        )
    }
}
