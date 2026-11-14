//! Complete immutable native input. Publication grants no delivery authority.
mod entry;
mod head;
mod receipt;
mod signal;
use super::metadata::{self, Budget, Publisher};
use agentchat_core::{
    Error, ErrorCode, Result,
    delivery::{MessageDispatch, MessageInput, NativeDispatch, NativeInput},
    model::{Context, HostKind},
    project::NativeFrameRevision,
};
pub use entry::InputEntry;
pub use head::InputHead;
pub use receipt::{InputReceipt, NativeInputResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use signal::{PendingInput, remove_scope, retire_scope};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(super) fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(required()),
    }
}

/// One file for one durable stream token, not a last-message latch. The native
/// consumer must independently check the current claim, root, original owner,
/// launch and controls before handing `text` to the host. Saving/reading this
/// file cannot advance an AGENTCHAT cursor or reads, start a model or renew authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFile {
    version: u32,
    /// Exact original host root, independently associated with the launched CLI.
    pub root: Uuid,
    /// Native callback reservation, separate from AGENTCHAT's launch UUID in the body.
    pub native_generation: u64,
    /// Exact original application routing context.
    pub context_id: Uuid,
    /// Application connection that claimed this input.
    pub incarnation: Uuid,
    /// Original durable message-head token; retries never edit its file.
    pub token: Uuid,
    /// Attributed whole-body native input, with no truncation or body-fetch hint.
    pub text: String,
}
impl InputFile {
    /// Native notification framing for the static asyncRewake path. A matching
    /// whole frame in the native core's kept queue row, not stderr emission,
    /// is required for acceptance. The dispatch token disambiguates retries.
    pub fn notice(&self) -> Result<String> {
        self.input()?;
        Ok(format!(
            "AGENTCHAT_INPUT_NOTICE root={} context={} token={}\n{}",
            self.root, self.context_id, self.token, self.text
        ))
    }
    /// Bound continuation of an already accepted native queue entry to its
    /// original complete bytes. This proof is not permission to resubmit it.
    pub fn notice_proof(&self) -> Result<([u8; 32], usize)> {
        let notice = self.notice()?;
        Ok((Sha256::digest(notice.as_bytes()).into(), notice.len()))
    }
    /// Render a claimed body without filesystem or native I/O. Caller must hold
    /// independent original launch/claim authority; construction does not prove it.
    pub fn new(
        context: &Context,
        root: Uuid,
        native_generation: u64,
        dispatch: &MessageDispatch,
    ) -> Result<Self> {
        Self::new_native(
            context,
            root,
            native_generation,
            &NativeDispatch::Message(dispatch.clone()),
        )
    }
    /// Render either native input kind without altering its canonical frame.
    pub fn new_native(
        context: &Context,
        root: Uuid,
        native_generation: u64,
        dispatch: &NativeDispatch,
    ) -> Result<Self> {
        if context.host != HostKind::Claude
            || dispatch.frame_revision() != NativeFrameRevision::DispatchV4
            || context.host_session_id != root.to_string()
            || context.project_id != dispatch.input().project_id()
            || !(1..=3).contains(&dispatch.attempt())
        {
            return Err(required());
        }
        let file = Self {
            version: 5,
            root,
            native_generation,
            context_id: context.context_id,
            incarnation: context.incarnation,
            token: dispatch.token(),
            text: dispatch.input().text()?,
        };
        file.input()?;
        Ok(file)
    }
    /// Decode either canonical native input frame.
    pub fn input(&self) -> Result<NativeInput> {
        self.validate_envelope()?;
        NativeInput::from_text(&self.text).map_err(|_| required())
    }
    fn validate_envelope(&self) -> Result<()> {
        if self.version != 5 {
            return Err(Error::new(
                ErrorCode::UnsupportedProtocol,
                "The native input capsule format is unsupported; use agentchat install --new-project for fresh setup.",
            ));
        }
        if [self.root, self.context_id, self.incarnation, self.token]
            .into_iter()
            .any(|id| id.is_nil())
            || self.native_generation == 0
            || self.native_generation > 9_007_199_254_740_991
            || self.text.len() > 65_536 + 8192
        {
            return Err(required());
        }
        Ok(())
    }
    // Original-result recovery preserves the exact current-format capsule.
    // Decoding does not grant a new native entry permit or submission.
    pub(super) fn original_input(&self, revision: NativeFrameRevision) -> Result<NativeInput> {
        self.validate_envelope()?;
        match revision {
            NativeFrameRevision::DispatchV4 => self.input(),
        }
    }
    fn retained_input(&self) -> Result<NativeInput> {
        self.input()
    }
    fn retained_revision(&self) -> Result<NativeFrameRevision> {
        self.validate_envelope()?;
        Ok(NativeFrameRevision::DispatchV4)
    }
    /// Only current payloads participate in the durable one-shot entry protocol.
    /// Old capsules grant neither recovery interpretation nor a fresh permit.
    pub fn supports_entry_claim(&self) -> bool {
        self.version == 5
    }
    /// Decode the original typed message, requiring its complete canonical
    /// rendering and independent native/application token fields to be valid.
    pub fn message(&self) -> Result<MessageInput> {
        match self.input()? {
            NativeInput::Message(input) => Ok(input),
            NativeInput::Notice(_) => Err(required()),
        }
    }
    /// Stable per-token path. Cwd, display names and lexical file order never
    /// select a recipient or replace the database's committed event sequence.
    pub fn path(&self, directory: &Path) -> PathBuf {
        directory.join(format!("input-{}-{}.json", self.context_id, self.token))
    }
    /// Durably publish through original parent/file/content provenance and
    /// tracked stage origins. An exact existing capsule is an idempotent retry;
    /// a foreign, edited, legacy, copied or symbolic entry is never adopted.
    /// Caller serializes this under its original native lifecycle lock.
    pub fn publish(&self, publisher: &Publisher, directory: &Path) -> Result<()> {
        self.input()?;
        let path = self.path(directory);
        if std::fs::symlink_metadata(&path).is_ok() {
            return metadata::sync_input(&path, self);
        }
        publisher.clone().input().create(&path, self)
    }
    /// Read one bounded, original-proved input capsule. This neither consumes
    /// it nor supplies a permanent permit after AGENTCHAT has accepted/retired its head.
    pub fn read(path: &Path) -> Result<Self> {
        let file: Self = metadata::read_budget(path, Budget::Input)?;
        file.input()?;
        if path.file_name() != file.path(path.parent().ok_or_else(required)?).file_name() {
            return Err(required());
        }
        Ok(file)
    }
    pub(in crate::claude) fn read_retained(path: &Path) -> Result<Self> {
        let file: Self = metadata::read_budget(path, Budget::Input)?;
        file.retained_input()?;
        if path.file_name() != file.path(path.parent().ok_or_else(required)?).file_name() {
            return Err(required());
        }
        Ok(file)
    }
    /// Remove only this exact still-owned input after the caller independently
    /// records native acceptance or terminal cancellation. Missing is harmless;
    /// changed/successor entries are preserved. No cursor or receipt is changed.
    pub fn remove(&self, directory: &Path) -> Result<bool> {
        self.input()?;
        metadata::remove_budget(&self.path(directory), self, Budget::Input)
    }
    pub(in crate::claude) fn remove_retained(&self, directory: &Path) -> Result<bool> {
        self.retained_input()?;
        metadata::remove_budget(&self.path(directory), self, Budget::Input)
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The complete original native input is unavailable or changed.",
    )
}

#[cfg(test)]
#[path = "../../tests/unit/native_body.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "../../tests/unit/native_input_receipts.rs"]
mod receipt_tests;

#[cfg(test)]
#[path = "../../tests/unit/native_original_frames.rs"]
mod original_frame_tests;
