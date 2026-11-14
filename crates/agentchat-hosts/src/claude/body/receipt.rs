//! A native result is compared against the whole original input before saving.
use super::{InputFile, metadata, required};
use agentchat_core::{Result, delivery::NativeInput, sequence::Sequence};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Result returned by a native SDK call or core queue, never a file-save ACK.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "route",
    content = "result",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeInputResult {
    /// A generic failure supplies no proof of non-entry; its negative result
    /// remains unknown and cannot authorize another submission.
    Failed,
    /// Explicit cancellation/refusal before the complete body entered the host.
    RejectedBeforeEntry,
    /// Whole user-row result from `session.append`, including its native UUID.
    Append(Value),
    /// Host core's kept queue row for the addressed asyncRewake whole body.
    /// A prompt hook's `{text}` return alone is not a native acceptance proof.
    Queued(Value),
}
/// Small immutable receipt for exactly one original full-body attempt. Only
/// explicit native success can set accepted; rejection never advances FIFO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputReceipt {
    version: u32,
    /// Exact original native conversation.
    pub root: Uuid,
    /// Original native callback reservation.
    pub native_generation: u64,
    /// Original immutable application connection.
    pub context_id: Uuid,
    /// Connection incarnation associated with the original head.
    pub incarnation: Uuid,
    /// Exact original dispatch token.
    pub token: Uuid,
    /// Current AGENTCHAT launch UUID, separate from native reservation generation.
    pub generation: Uuid,
    /// Original stable recipient message delivery.
    pub delivery_id: Uuid,
    /// Native acceptance order follows this committed project sequence.
    pub seq: Sequence,
    digest: [u8; 32],
    /// True only after full native result comparison. A v1 false value is
    /// ambiguous; only v2 false values prove rejection before entry.
    pub accepted: bool,
    native_id: Option<Uuid>,
}
impl InputReceipt {
    /// Verify a native result without waiting for a model, reading or task
    /// action. This does not prove the caller's root/process/claim authority;
    /// the native-only producer independently checks those before publication.
    pub fn from_native(file: &InputFile, result: NativeInputResult) -> Result<Self> {
        let input = file.input()?;
        let version = if matches!(&result, NativeInputResult::Failed) {
            1
        } else {
            2
        };
        let (accepted, native_id) = match result {
            NativeInputResult::Failed | NativeInputResult::RejectedBeforeEntry => (false, None),
            NativeInputResult::Append(value) if !input.addressed() => {
                if value.get("deny").is_some_and(Value::is_string) {
                    (false, None)
                } else {
                    let message = &value["message"];
                    let id = value["uuid"]
                        .as_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .filter(|id| !id.is_nil())
                        .ok_or_else(required)?;
                    if message["type"] != "user"
                        || message["role"] != "user"
                        || message["isMeta"] != true
                        || message["content"]
                            != serde_json::json!([{"type":"text","text":file.text}])
                        || value.get("deny").is_some()
                    {
                        return Err(required());
                    }
                    (true, Some(id))
                }
            }
            NativeInputResult::Queued(value) if input.addressed() => {
                let id = value["uuid"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .filter(|id| !id.is_nil())
                    .ok_or_else(required)?;
                let kept = &value["result"];
                let notice = file.notice()?;
                let blocks = kept["message"]["content"].as_array().ok_or_else(required)?;
                if value["root"] != file.root.to_string()
                    || value["door"] != "queue"
                    || value["origin"]["kind"] != "engine"
                    || value["core"]["plugin"] != "engine"
                    || value["core"]["tier"] != "core"
                    || kept["uuid"] != id.to_string()
                    || kept["message"]["type"] != "user"
                    || kept["message"]["role"] != "user"
                    || blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .map(|b| {
                            b["text"]
                                .as_str()
                                .map_or(0, |text| text.matches(&notice).count())
                        })
                        .sum::<usize>()
                        != 1
                {
                    return Err(required());
                }
                (true, Some(id))
            }
            _ => return Err(required()),
        };
        Ok(Self {
            version,
            root: file.root,
            native_generation: file.native_generation,
            context_id: file.context_id,
            incarnation: file.incarnation,
            token: file.token,
            generation: input.generation(),
            delivery_id: input.delivery_id(),
            seq: input.seq(),
            digest: Sha256::digest(file.text.as_bytes()).into(),
            accepted,
            native_id,
        })
    }
    /// Pure correlation against the original immutable full input, including
    /// launch, message delivery, sequence, token and complete text checksum.
    pub fn matches(&self, file: &InputFile) -> Result<bool> {
        let input = file.input()?;
        self.matches_input(file, &input)
    }
    pub(super) fn matches_retained(&self, file: &InputFile) -> Result<bool> {
        let input = file.retained_input()?;
        self.matches_input(file, &input)
    }
    fn matches_input(&self, file: &InputFile, input: &NativeInput) -> Result<bool> {
        self.validate()?;
        Ok(self.root == file.root
            && self.native_generation == file.native_generation
            && self.context_id == file.context_id
            && self.incarnation == file.incarnation
            && self.token == file.token
            && self.generation == input.generation()
            && self.delivery_id == input.delivery_id()
            && self.seq == input.seq()
            && self.digest == <[u8; 32]>::from(Sha256::digest(file.text.as_bytes())))
    }
    pub(super) fn validate(&self) -> Result<()> {
        if !matches!(self.version, 1 | 2)
            || [
                self.root,
                self.context_id,
                self.incarnation,
                self.token,
                self.generation,
                self.delivery_id,
            ]
            .into_iter()
            .any(|v| v.is_nil())
            || self.native_generation == 0
            || self.native_generation > 9_007_199_254_740_991
            || self.native_id.is_some_and(|id| id.is_nil())
            || self.accepted != self.native_id.is_some()
        {
            return Err(required());
        }
        Ok(())
    }
    /// Only v2 negatives distinguish proved non-entry from legacy generic SDK
    /// failures. An old negative cannot manufacture a replay permission.
    pub fn rejected_before_entry(&self) -> bool {
        self.version == 2 && !self.accepted
    }
    /// Stable receipt name, separate from any successor head's mutable locator.
    pub fn path(directory: &Path, context: Uuid, token: Uuid) -> PathBuf {
        directory.join(format!("accepted-input-{context}-{token}.json"))
    }
    /// Read original-proved bounded metadata; a similarly named legacy JSON
    /// document cannot manufacture native acceptance.
    pub fn read(path: &Path) -> Result<Self> {
        let value: Self = metadata::read_proved(path)?;
        value.validate()?;
        if path.file_name()
            != Self::path(
                path.parent().ok_or_else(required)?,
                value.context_id,
                value.token,
            )
            .file_name()
        {
            return Err(required());
        }
        Ok(value)
    }
    /// Publish one original receipt exclusively. An exact retry preserves its
    /// original inode and parent durability; a different/foreign result is kept.
    pub fn publish(&self, publisher: &metadata::Publisher, directory: &Path) -> Result<()> {
        self.validate()?;
        let path = Self::path(directory, self.context_id, self.token);
        if std::fs::symlink_metadata(&path).is_ok() {
            return metadata::sync_input(&path, self);
        }
        publisher.create(&path, self)
    }
    /// Exact original removal under the caller's lifecycle/retirement lock.
    pub fn remove(&self, directory: &Path) -> Result<bool> {
        self.validate()?;
        metadata::remove_if(&Self::path(directory, self.context_id, self.token), self)
    }
}
