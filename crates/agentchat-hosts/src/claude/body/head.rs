//! Bounded original head lookup survives a same-owner native connection change.
use super::{InputEntry, InputFile, InputReceipt, exists, metadata, required};
use agentchat_core::{Result, project::NativeFrameRevision};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// One original-proved locator per physical owner, never an independent FIFO.
/// Only an application-committed successor may replace it. Native scope retirement
/// preserves this original body/result for read-only recovery; it grants no input
/// permission and cannot restore a retired application launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputHead {
    version: u32,
    root: Uuid,
    native_generation: u64,
    context_id: Uuid,
    incarnation: Uuid,
    token: Uuid,
    generation: Uuid,
}
impl InputHead {
    /// Capture the exact already committed full-body token before notification.
    pub fn new(file: &InputFile) -> Result<Self> {
        Self::with_generation(file, file.input()?.generation())
    }
    pub(super) fn new_retained(file: &InputFile) -> Result<Self> {
        Self::with_generation(file, file.retained_input()?.generation())
    }
    fn with_generation(file: &InputFile, generation: Uuid) -> Result<Self> {
        Ok(Self {
            version: 1,
            root: file.root,
            native_generation: file.native_generation,
            context_id: file.context_id,
            incarnation: file.incarnation,
            token: file.token,
            generation,
        })
    }
    pub(super) fn validate(&self) -> Result<()> {
        if self.version != 1
            || [
                self.root,
                self.context_id,
                self.incarnation,
                self.token,
                self.generation,
            ]
            .into_iter()
            .any(|id| id.is_nil())
            || self.native_generation == 0
            || self.native_generation > 9_007_199_254_740_991
        {
            return Err(required());
        }
        Ok(())
    }
    /// Fixed-size lookup; directory order and the most recent filename are unused.
    pub fn path(directory: &Path) -> PathBuf {
        directory.join("input-head.json")
    }
    /// Read the original bounded locator, preserving changed/foreign records.
    pub fn read(directory: &Path) -> Result<Option<Self>> {
        let path = Self::path(directory);
        if !exists(&path)? {
            return Ok(None);
        }
        let value: Self = metadata::read_proved(&path)?;
        value.validate()?;
        Ok(Some(value))
    }
    /// Compare the complete immutable body provenance, independently of current
    /// connection authority. Equality never authorizes another native submission.
    pub fn matches(&self, file: &InputFile) -> Result<bool> {
        self.validate()?;
        Ok(*self == Self::new(file)?)
    }
    /// Correlate a receipt-only attempt across connections before opening a body.
    /// Complete text/provenance validation remains mandatory after this lookup.
    pub fn same_attempt(&self, file: &InputFile) -> Result<bool> {
        self.validate()?;
        Ok(self.root == file.root
            && self.token == file.token
            && self.generation == file.input()?.generation())
    }
    pub(super) fn matches_pending(&self, pending: &super::PendingInput) -> bool {
        self.root == pending.root
            && self.native_generation == pending.native_generation
            && self.context_id == pending.context_id
            && self.incarnation == pending.incarnation
            && self.token == pending.token
    }
    pub(super) fn token_key(&self) -> (Uuid, Uuid) {
        (self.context_id, self.token)
    }
    /// Original native reservation floor for bounded dead-owner collection.
    pub fn native_generation(&self) -> u64 {
        self.native_generation
    }
    /// Recover this exact original body without reading a directory or a successor.
    pub fn input(&self, directory: &Path) -> Result<InputFile> {
        self.validate()?;
        let file = InputFile::read(
            &directory.join(format!("input-{}-{}.json", self.context_id, self.token)),
        )?;
        if !self.matches(&file)? {
            return Err(required());
        }
        Ok(file)
    }
    // Retained bytes are read only for an original result or independently
    // fenced cleanup. This path never grants publication/load/entry permission.
    fn retained_input(&self, directory: &Path) -> Result<InputFile> {
        self.validate()?;
        let file = InputFile::read_retained(
            &directory.join(format!("input-{}-{}.json", self.context_id, self.token)),
        )?;
        if *self != Self::new_retained(&file)? {
            return Err(required());
        }
        Ok(file)
    }
    pub(in crate::claude) fn original_input(
        &self,
        directory: &Path,
        revision: NativeFrameRevision,
    ) -> Result<InputFile> {
        let file = self.retained_input(directory)?;
        if file.retained_revision()? != revision {
            return Err(required());
        }
        Ok(file)
    }
    /// Read only an exact original native result, including whole-body checksum.
    pub fn receipt(&self, directory: &Path, file: &InputFile) -> Result<Option<InputReceipt>> {
        if !self.matches(file)? {
            return Err(required());
        }
        let path = InputReceipt::path(directory, self.context_id, self.token);
        if !exists(&path)? {
            return Ok(None);
        }
        let value = InputReceipt::read(&path)?;
        if !value.matches(file)? {
            return Err(required());
        }
        Ok(Some(value))
    }
    pub(in crate::claude) fn original_receipt(
        &self,
        directory: &Path,
        file: &InputFile,
        revision: NativeFrameRevision,
    ) -> Result<Option<InputReceipt>> {
        self.validate()?;
        if file.retained_revision()? != revision || *self != Self::new_retained(file)? {
            return Err(required());
        }
        let path = InputReceipt::path(directory, self.context_id, self.token);
        if !exists(&path)? {
            return Ok(None);
        }
        let value = InputReceipt::read(&path)?;
        if !value.matches_retained(file)? {
            return Err(required());
        }
        Ok(Some(value))
    }
    /// Publish exclusively after the original body and before its wake signal.
    /// An exact retry syncs the original record; replacement needs explicit cleanup.
    pub fn publish(&self, publisher: &metadata::Publisher, directory: &Path) -> Result<()> {
        self.validate()?;
        let path = Self::path(directory);
        if exists(&path)? {
            return metadata::sync_input(&path, self);
        }
        publisher.create(&path, self)
    }
    /// Remove this locator only, under an independently held lifecycle/GC lock.
    pub fn remove(&self, directory: &Path) -> Result<bool> {
        self.validate()?;
        metadata::remove_if(&Self::path(directory), self)
    }
    /// Retire at most four original transport files after a committed successor.
    /// Partial prefixes are recoverable; no application cursor or history changes.
    pub fn remove_transport(&self, directory: &Path) -> Result<usize> {
        self.validate()?;
        if Self::read(directory)?.as_ref() != Some(self) {
            return Err(required());
        }
        let path = directory.join(format!("input-{}-{}.json", self.context_id, self.token));
        let file = if exists(&path)? {
            Some(self.retained_input(directory)?)
        } else {
            None
        };
        let receipt_path = InputReceipt::path(directory, self.context_id, self.token);
        let mut count = 0;
        if exists(&receipt_path)? {
            let receipt = InputReceipt::read(&receipt_path)?;
            if receipt.root != self.root
                || receipt.native_generation != self.native_generation
                || receipt.context_id != self.context_id
                || receipt.incarnation != self.incarnation
                || receipt.token != self.token
                || receipt.generation != self.generation
                || file
                    .as_ref()
                    .is_some_and(|file| !receipt.matches_retained(file).unwrap_or(false))
            {
                return Err(required());
            }
            count += usize::from(receipt.remove(directory)?);
        }
        let entry_path = directory.join(format!(
            "entered-input-{}-{}.json",
            self.context_id, self.token
        ));
        if exists(&entry_path)? {
            let entry = InputEntry::read(&entry_path)?;
            if !entry.matches_head(self)
                || file
                    .as_ref()
                    .is_some_and(|file| !entry.matches_retained(file).unwrap_or(false))
            {
                return Err(required());
            }
            count += usize::from(entry.remove(directory)?);
        }
        if let Some(file) = file {
            count += usize::from(file.remove_retained(directory)?);
        }
        count += usize::from(self.remove(directory)?);
        Ok(count)
    }
}
