//! Surviving immutable results and one private recoverable removal intention.
use super::{
    conflict, io as files,
    plan::{Plan, Request},
};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::removal::RemovalOutcome;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completed {
    version: u32,
    request: Request,
    outcome: RemovalOutcome,
}
pub(super) struct Journal {
    pub plan: Plan,
    raw: Vec<u8>,
    path: PathBuf,
}
impl Journal {
    pub fn load(directory: &Path) -> Result<Option<Self>> {
        let path = directory.join("pending-removal.json");
        let Some(raw) = files::read(&path)? else {
            return Ok(None);
        };
        let plan = serde_json::from_slice(&raw)
            .map_err(|_| conflict("Invalid removal journal format."))?;
        Ok(Some(Self { plan, raw, path }))
    }
    pub fn begin(directory: &Path, plan: Plan) -> Result<Self> {
        let path = directory.join("pending-removal.json");
        let raw = encode(&plan)?;
        files::write(&path, &raw, None)?;
        Ok(Self { plan, raw, path })
    }
    pub fn save(&mut self) -> Result<()> {
        let next = encode(&self.plan)?;
        files::write(&self.path, &next, Some(&self.raw))?;
        self.raw = next;
        Ok(())
    }
    pub fn clear(&self) -> Result<()> {
        files::clear(&self.path, &self.raw)
    }
}
fn receipt(directory: &Path, request: &Request) -> PathBuf {
    directory
        .join("removal-results")
        .join(format!("{}.json", request.id))
}
pub(super) fn completed(directory: &Path, request: &Request) -> Result<Option<RemovalOutcome>> {
    let Some(raw) = files::read(&receipt(directory, request))? else {
        return Ok(None);
    };
    let value: Completed = serde_json::from_slice(&raw)
        .map_err(|_| conflict("Invalid removal completion receipt."))?;
    if value.version != 1 || value.request != *request {
        return Err(identity_conflict());
    }
    if value.outcome.scope != request.scope
        || value.outcome.recovery_directory.native_bytes != directory.as_os_str().as_encoded_bytes()
    {
        return Err(conflict(
            "Removal receipt has inconsistent result authority.",
        ));
    }
    Ok(Some(value.outcome))
}
pub(super) fn finish(directory: &Path, request: &Request, outcome: &RemovalOutcome) -> Result<()> {
    let parent = directory.join("removal-results");
    files::private_dir(&parent)?;
    let bytes = encode(&Completed {
        version: 1,
        request: request.clone(),
        outcome: outcome.clone(),
    })?;
    let path = receipt(directory, request);
    if let Some(existing) = files::read(&path)? {
        if existing != bytes {
            return Err(conflict(
                "Removal result differs from an already committed receipt.",
            ));
        }
        files::Directory::open(&parent)?.sync()?;
        return Ok(());
    }
    files::write(&path, &bytes, None)
}
pub(super) fn identity_conflict() -> Error {
    Error::new(
        ErrorCode::IdempotencyConflict,
        "Uninstall request ID already belongs to another scope, directory or selected-root configuration.",
    )
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| conflict("Cannot encode removal metadata."))?;
    bytes.push(b'\n');
    if bytes.len() > files::LIMIT {
        return Err(conflict("Removal journal exceeds its bounded size."));
    }
    Ok(bytes)
}
