//! Read-only signal validation for physical retention; never starts a lifecycle.
use super::*;
use crate::claude::metadata::directory::{FileId, Snapshot};

pub(crate) fn location(home: &Path, cwd: &Path) -> PathBuf {
    super::location(home, cwd)
}

pub(crate) fn birth(
    raw: &[u8],
    lock: FileId,
    cwd: &Path,
    signal: FileId,
    stage: &std::ffi::OsStr,
) -> Result<bool> {
    genesis::validate_recovery(raw, lock, cwd, signal, stage)
}

/// Observe a completed or interrupted original signal without repairing it.
pub(crate) fn history(
    anchor_path: &Path,
    cwd: &Path,
    signal: &Snapshot,
    sealed: bool,
) -> Result<()> {
    if !exists(anchor_path)? {
        return if !sealed && signal.bytes == b"0\n" {
            Ok(())
        } else {
            Err(required())
        };
    }
    let anchor: Anchor = metadata::read(anchor_path)?;
    if anchor.version != 2 || anchor.cwd != cwd || FileId(anchor.dev, anchor.ino) != signal.id {
        return Err(required());
    }
    counter::validate(&anchor)?;
    if !sealed && (anchor.floor != 0 || anchor.pending.is_some() || signal.bytes != b"0\n") {
        return Err(required());
    }
    counter::observe(&anchor, &signal.bytes)
}
