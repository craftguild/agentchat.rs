//! One locator for the database-owned head; immutable payloads carry each body.
use super::{InputEntry, InputFile, InputHead, InputReceipt, metadata, required};
use crate::claude::NativeRegistration;
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Small original-proved locator, never a file-based replacement for AGENTCHAT FIFO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingInput {
    version: u32,
    /// Exact native root.
    pub root: Uuid,
    /// Native callback generation, not AGENTCHAT launch generation.
    pub native_generation: u64,
    /// Original application context.
    pub context_id: Uuid,
    /// Original application connection.
    pub incarnation: Uuid,
    /// One already committed head token.
    pub token: Uuid,
    /// Existing cwd wake-counter generation; not an event sequence.
    pub signal: u64,
}
/// Retire native output permission while retaining the one original body/result
/// for a receipt-only poll after same-root reconnection. Pending native callbacks
/// lose their locator; positive root/launch fences still prevent old delivery.
pub fn retire_scope(directory: &Path, scope: &NativeRegistration) -> Result<usize> {
    let path = PendingInput::path(directory, scope.context.context_id);
    if super::exists(&path)? {
        let pending = PendingInput::read(&path)?;
        if pending.root != scope.root
            || pending.native_generation != scope.generation
            || pending.incarnation != scope.context.incarnation
        {
            return Err(required());
        }
        if let Some(head) = InputHead::read(directory)?
            && head == InputHead::new_retained(&pending.retained_input(directory)?)?
        {
            return Ok(usize::from(pending.remove(directory)?));
        }
    }
    remove_scope(directory, scope)
}
impl PendingInput {
    /// Associate an immutable full input with one existing notification signal.
    pub fn new(file: &InputFile, signal: u64) -> Result<Self> {
        file.input()?;
        let value = Self {
            version: 1,
            root: file.root,
            native_generation: file.native_generation,
            context_id: file.context_id,
            incarnation: file.incarnation,
            token: file.token,
            signal,
        };
        value.validate()?;
        Ok(value)
    }
    /// Pure complete locator correlation, without native or application effects.
    pub fn matches(&self, file: &InputFile) -> bool {
        self.root == file.root
            && self.native_generation == file.native_generation
            && self.context_id == file.context_id
            && self.incarnation == file.incarnation
            && self.token == file.token
    }
    pub(super) fn validate(&self) -> Result<()> {
        if self.version != 1
            || [self.root, self.context_id, self.incarnation, self.token]
                .into_iter()
                .any(|id| id.is_nil())
            || self.native_generation == 0
            || self.native_generation > 9_007_199_254_740_991
            || self.signal == 0
            || self.signal > 9_007_199_254_740_991
        {
            return Err(required());
        }
        Ok(())
    }
    /// Fixed per-context locator; its contents never choose lexical FIFO order.
    pub fn path(directory: &Path, context: Uuid) -> PathBuf {
        directory.join(format!("pending-input-{context}.json"))
    }
    /// Read an original-proved bounded locator without adopting legacy JSON.
    pub fn read(path: &Path) -> Result<Self> {
        let value: Self = metadata::read_proved(path)?;
        value.validate()?;
        if path.file_name()
            != Self::path(path.parent().ok_or_else(required)?, value.context_id).file_name()
        {
            return Err(required());
        }
        Ok(value)
    }
    /// Read exactly this immutable body; no directory scan or last-body choice.
    pub fn input(&self, directory: &Path) -> Result<InputFile> {
        self.validate()?;
        let path = directory.join(format!("input-{}-{}.json", self.context_id, self.token));
        let file = InputFile::read(&path)?;
        if !self.matches(&file) {
            return Err(required());
        }
        Ok(file)
    }
    fn retained_input(&self, directory: &Path) -> Result<InputFile> {
        self.validate()?;
        let path = directory.join(format!("input-{}-{}.json", self.context_id, self.token));
        let file = InputFile::read_retained(&path)?;
        if !self.matches(&file) {
            return Err(required());
        }
        Ok(file)
    }
    /// Replace only an original-proved locator under the native lifecycle lock.
    /// The application must have committed the corresponding head before this.
    pub fn publish(&self, publisher: &metadata::Publisher, directory: &Path) -> Result<()> {
        self.validate()?;
        publisher.write(&Self::path(directory, self.context_id), self)
    }
    /// Remove only this exact locator under the caller's lifecycle lock.
    pub fn remove(&self, directory: &Path) -> Result<bool> {
        self.validate()?;
        metadata::remove_if(&Self::path(directory, self.context_id), self)
    }
}
/// Remove at most four proved transport files for one exact retired scope.
/// The caller holds the original lifecycle lock; foreign/changed data survives.
/// No shared history, application cursor, receipt or signal is modified.
pub fn remove_scope(directory: &Path, scope: &NativeRegistration) -> Result<usize> {
    let path = PendingInput::path(directory, scope.context.context_id);
    if !super::exists(&path)? {
        return Ok(0);
    }
    let pending = PendingInput::read(&path)?;
    if pending.root != scope.root
        || pending.native_generation != scope.generation
        || pending.incarnation != scope.context.incarnation
    {
        return Err(required());
    }
    let file_path = directory.join(format!(
        "input-{}-{}.json",
        pending.context_id, pending.token
    ));
    let file = if super::exists(&file_path)? {
        Some(pending.retained_input(directory)?)
    } else {
        None
    };
    let receipt_path = InputReceipt::path(directory, pending.context_id, pending.token);
    let mut removed = 0;
    if super::exists(&receipt_path)? {
        let receipt = InputReceipt::read(&receipt_path)?;
        if receipt.root != pending.root
            || receipt.native_generation != pending.native_generation
            || receipt.context_id != pending.context_id
            || receipt.incarnation != pending.incarnation
            || receipt.token != pending.token
            || file
                .as_ref()
                .is_some_and(|file| !receipt.matches_retained(file).unwrap_or(false))
        {
            return Err(required());
        }
        removed += usize::from(receipt.remove(directory)?);
    }
    let entry_path = directory.join(format!(
        "entered-input-{}-{}.json",
        pending.context_id, pending.token
    ));
    if super::exists(&entry_path)? {
        let entry = InputEntry::read(&entry_path)?;
        if !entry.matches_pending(&pending)
            || file
                .as_ref()
                .is_some_and(|file| !entry.matches_retained(file).unwrap_or(false))
        {
            return Err(required());
        }
        removed += usize::from(entry.remove(directory)?);
    }
    if let Some(file) = file {
        removed += usize::from(file.remove_retained(directory)?);
    }
    removed += usize::from(pending.remove(directory)?);
    Ok(removed)
}
