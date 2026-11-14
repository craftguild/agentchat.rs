//! Finite private projections shared by retirement and dead-owner collection.
use super::{snapshot as files, unavailable};
use crate::claude::{
    NativeLifecycle, NativeRegistration,
    process::ProcessIdentity,
    wake::{Submission, WakeRecord},
};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Native callback facts paired with its independently pinned kernel owner.
/// This private record never establishes current root or host authority alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleOwner {
    /// Immutable bootstrap or project lifecycle supplied by a native callback.
    pub registration: NativeLifecycle,
    /// Full native CLI process identity; never a PID-only root association.
    pub owner: ProcessIdentity,
}
/// Remove only an exact registration's bounded wake/submission projections.
/// Call while holding its live retirement fence or validated dead-owner lock.
/// Both writers use the same owner lock. Missing files permit partial retry;
/// changed identities are preserved. This never changes delivery/ACK/read history.
pub fn remove_notices(path: &Path, registration: &NativeRegistration) -> Result<usize> {
    remove_notices_in(path, registration, files::directory(path)?)
}
pub(super) fn remove_notices_in(
    path: &Path,
    registration: &NativeRegistration,
    directory: (u64, u64),
) -> Result<usize> {
    if files::directory(path)? != directory {
        return Err(unavailable());
    }
    let context = &registration.context;
    let wake_path = path.join(format!("wake-{}.json", context.context_id));
    let mut removed = 0;
    if files::exists(&wake_path)? {
        let (wake, stamp): (WakeRecord, _) = files::small_record(&wake_path)?;
        // Signal order belongs to the cwd, independently of lifecycle generation.
        if stamp.in_directory(directory)
            && wake.root == registration.root
            && wake.context_id == context.context_id
            && wake.incarnation == context.incarnation
        {
            removed += usize::from(stamp.remove(&wake_path)?);
        }
    }
    let receipt_path = path.join(format!("submitted-{}.json", context.context_id));
    if files::exists(&receipt_path)? {
        let (receipt, stamp): (Submission, _) = files::small_record(&receipt_path)?;
        if stamp.in_directory(directory)
            && receipt.root == registration.root
            && receipt.context_id == context.context_id
            && receipt.incarnation == context.incarnation
        {
            removed += usize::from(stamp.remove(&receipt_path)?);
        }
    }
    Ok(removed)
}
