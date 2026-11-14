//! A durable one-shot native entry permit, never evidence of acceptance.
use super::{InputFile, InputHead, metadata, required};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Original-proved intent written before the host call. Its presence prevents
/// another callback/module instance from entering the same full body again.
/// Crash before actual entry may conservatively stall this original attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputEntry {
    version: u32,
    head: InputHead,
    digest: [u8; 32],
}
impl InputEntry {
    fn new(file: &InputFile) -> Result<Self> {
        Ok(Self {
            version: 1,
            head: InputHead::new(file)?,
            digest: Sha256::digest(file.text.as_bytes()).into(),
        })
    }
    fn path(&self, directory: &Path) -> PathBuf {
        let (context, token) = self.head.token_key();
        directory.join(format!("entered-input-{context}-{token}.json"))
    }
    /// Locate only the exact attempt, never a directory-order or cwd identity.
    pub fn path_for(directory: &Path, file: &InputFile) -> Result<PathBuf> {
        Ok(Self::new(file)?.path(directory))
    }
    /// Read original parent/inode/content provenance; a copied/edited intent
    /// cannot be adopted as either a permit or a result.
    pub fn read(path: &Path) -> Result<Self> {
        let value: Self = metadata::read_proved(path)?;
        value.head.validate()?;
        if value.version != 1
            || path.file_name() != value.path(path.parent().ok_or_else(required)?).file_name()
        {
            return Err(required());
        }
        Ok(value)
    }
    /// Correlate the complete original immutable body and native reservation.
    pub fn matches(&self, file: &InputFile) -> Result<bool> {
        Ok(*self == Self::new(file)?)
    }
    pub(super) fn matches_retained(&self, file: &InputFile) -> Result<bool> {
        Ok(self.version == 1
            && self.head == InputHead::new_retained(file)?
            && self.digest == <[u8; 32]>::from(Sha256::digest(file.text.as_bytes())))
    }
    /// Correlate retained transport cleanup independently of payload presence.
    pub fn matches_head(&self, head: &InputHead) -> bool {
        self.head == *head
    }
    /// Correlate all pending scope fields before removing a partial prefix.
    pub fn matches_pending(&self, pending: &super::PendingInput) -> bool {
        self.head.matches_pending(pending)
    }
    /// Original native reservation floor for positively proved owner collection.
    pub fn native_generation(&self) -> u64 {
        self.head.native_generation()
    }
    /// Claim once under the caller's original lifecycle lock. Existing matching
    /// intent returns false; it never renews input authority or permits replay.
    pub fn claim(
        file: &InputFile,
        publisher: &metadata::Publisher,
        directory: &Path,
    ) -> Result<bool> {
        if !file.supports_entry_claim() {
            return Err(required());
        }
        let value = Self::new(file)?;
        let path = value.path(directory);
        if super::exists(&path)? {
            if Self::read(&path)? != value {
                return Err(required());
            }
            return Ok(false);
        }
        publisher.create(&path, &value)?;
        Ok(true)
    }
    /// Remove only a still-original intent during fenced successor/owner cleanup.
    pub fn remove(&self, directory: &Path) -> Result<bool> {
        metadata::remove_if(&self.path(directory), self)
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/native_input_entry.rs"]
mod tests;
