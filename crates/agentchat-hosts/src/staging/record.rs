//! Self-bound index records describe intent; only complete data proof permits work.
use super::{StagePurpose, unavailable};
use crate::claude::{
    metadata::directory::{Directory, FileId, Snapshot},
    process::ProcessIdentity,
};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs::Metadata,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Parent {
    pub path: Vec<u8>,
    pub id: FileId,
    pub alternate: Option<Vec<u8>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileProof {
    pub id: FileId,
    pub length: u64,
    pub changed: (i64, i64),
    pub modified: (i64, i64),
    pub digest: [u8; 32],
}
impl FileProof {
    pub fn snapshot(value: &Snapshot) -> Self {
        Self {
            id: value.id,
            length: value.bytes.len() as u64,
            changed: value.changed,
            modified: value.modified,
            digest: Sha256::digest(&value.bytes).into(),
        }
    }
}
impl FileProof {
    pub fn capture(m: &Metadata, bytes: &[u8]) -> Self {
        Self {
            id: FileId::of(m),
            length: bytes.len() as u64,
            changed: (m.ctime(), m.ctime_nsec()),
            modified: (m.mtime(), m.mtime_nsec()),
            digest: Sha256::digest(bytes).into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    pub parent: Parent,
    pub stage: Vec<u8>,
    pub target: Vec<u8>,
    pub purpose: StagePurpose,
    pub proof: Option<FileProof>,
    pub guard: Option<Guard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related: Option<Related>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocation: Option<super::allocation::model::Allocation>,
}
/// A full companion proof survives retirement of its separate index entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Related {
    pub name: Vec<u8>,
    pub parent: Parent,
    pub stage: Vec<u8>,
    pub target: Vec<u8>,
    pub purpose: StagePurpose,
    pub proof: FileProof,
}
impl Related {
    pub fn capture(value: &Record) -> Result<Self> {
        Ok(Self {
            name: value.name.clone(),
            parent: value.intent.parent.clone(),
            stage: value.intent.stage.clone(),
            target: value.intent.target.clone(),
            purpose: value.intent.purpose,
            proof: value.intent.proof.clone().ok_or_else(unavailable)?,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Guard {
    pub name: Vec<u8>,
    pub id: FileId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub version: u32,
    pub index: FileId,
    pub file: FileId,
    pub name: Vec<u8>,
    pub temporary: Vec<u8>,
    pub creator: ProcessIdentity,
    pub intent: Intent,
    #[serde(default, skip_serializing_if = "is_false")]
    pub retiring: bool,
    pub checksum: [u8; 32],
}
fn is_false(value: &bool) -> bool {
    !value
}
pub(super) fn path(bytes: &[u8]) -> PathBuf {
    OsString::from_vec(bytes.into()).into()
}
pub(super) fn component(bytes: &[u8]) -> Result<&OsStr> {
    if bytes.is_empty()
        || bytes.len() > 255
        || bytes.contains(&0)
        || bytes.contains(&b'/')
        || bytes == b"."
        || bytes == b".."
    {
        return Err(unavailable());
    }
    Ok(OsStr::from_bytes(bytes))
}
pub(super) fn absolute(path: &Path) -> Result<Vec<u8>> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        || path.as_os_str().as_bytes().len() > 16_384
        || path.as_os_str().as_bytes().contains(&0)
    {
        return Err(unavailable());
    }
    Ok(path.as_os_str().as_bytes().into())
}
pub(super) fn encode(value: &Record) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| unavailable())?;
    if bytes.len() > 65_536 {
        return Err(unavailable());
    }
    Ok(bytes)
}
impl Record {
    fn digest(&self) -> Result<[u8; 32]> {
        let mut value = self.clone();
        value.checksum = [0; 32];
        Ok(Sha256::new()
            .chain_update(b"agentchat/stage-origin/v1\0")
            .chain_update(encode(&value)?)
            .finalize()
            .into())
    }
    pub fn seal(&mut self) -> Result<Vec<u8>> {
        self.checksum = self.digest()?;
        encode(self)
    }
    pub fn decode(index: &Directory, name: &OsStr, snapshot: &Snapshot) -> Result<Self> {
        let value: Self = serde_json::from_slice(&snapshot.bytes).map_err(|_| unavailable())?;
        if value.version != 1
            || value.index != index.id
            || value.file != snapshot.id
            || encode(&value)? != snapshot.bytes
            || value.checksum != value.digest()?
            || (name.as_bytes() != value.name && name.as_bytes() != value.temporary)
        {
            return Err(unavailable());
        }
        component(&value.name)?;
        component(&value.temporary)?;
        // The self-bound frame may be observed at its exact canonical name or
        // its recorded complete swap. Neither a copy nor an arbitrary prefix
        // can turn a JSON body into an owned index entry.
        for (bytes, prefix, suffix) in [
            (&value.name, "", ".json"),
            (&value.temporary, ".origin-", ""),
        ] {
            let text = std::str::from_utf8(bytes).map_err(|_| unavailable())?;
            let raw = text
                .strip_prefix(prefix)
                .and_then(|v| v.strip_suffix(suffix))
                .ok_or_else(unavailable)?;
            let id = uuid::Uuid::parse_str(raw).map_err(|_| unavailable())?;
            if id.is_nil() || id.to_string() != raw {
                return Err(unavailable());
            }
        }
        component(&value.intent.stage)?;
        component(&value.intent.target)?;
        if let Some(allocation) = &value.intent.allocation {
            allocation.validate()?;
            if !matches!(
                value.intent.purpose,
                StagePurpose::Gateway | StagePurpose::Relay
            ) || value.intent.proof.is_some()
                || value.intent.related.is_some()
                || value.intent.guard.is_some()
                || value.intent.parent.alternate.is_some()
            {
                return Err(unavailable());
            }
        }
        if let Some(related) = &value.intent.related {
            component(&related.name)?;
            component(&related.stage)?;
            component(&related.target)?;
            absolute(&path(&related.parent.path))?;
            if related.name == value.name
                || related.stage == related.target
                || related.parent.alternate.is_some()
                || related.proof.length > related.purpose.limit() as u64
                || !matches!(
                    (value.intent.purpose, related.purpose),
                    (StagePurpose::Signal, StagePurpose::SignalLock)
                        | (StagePurpose::SignalLock, StagePurpose::Signal)
                )
            {
                return Err(unavailable());
            }
        }
        if let Some(guard) = &value.intent.guard {
            component(&guard.name)?;
            if guard.name == value.intent.stage || guard.name == value.intent.target {
                return Err(unavailable());
            }
        }
        absolute(&path(&value.intent.parent.path))?;
        if let Some(alias) = &value.intent.parent.alternate {
            absolute(&path(alias))?;
        }
        if value.intent.stage == value.intent.target
            || value
                .intent
                .proof
                .as_ref()
                .is_some_and(|p| p.length > value.intent.purpose.limit() as u64)
        {
            return Err(unavailable());
        }
        Ok(value)
    }
}
