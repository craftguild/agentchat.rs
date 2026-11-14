//! Private bounded fixture routing metadata; no message or credential storage.
pub(super) use agentchat_hosts::claude::observer::{NativeProfile, RegisteredOwner as NativeOwner};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub(super) type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    pub owner_root: Uuid,
    pub observer: String,
    pub generation: u64,
    pub root: Uuid,
    pub cwd: PathBuf,
    pub sequence: Option<u64>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
}
pub(super) use agentchat_cli::claude_gateway::records::{
    Control, Endpoint, LifecycleOwner, Request, Response, current_owner, endpoint, read, write,
};

pub(super) fn isolated(root: &Path) -> Result<()> {
    for (key, tail) in [
        ("AGENTCHAT_HOME", "state"),
        ("AGENTCHAT_USER_CONFIG_ROOT", "config"),
    ] {
        let actual = std::env::var_os(key).ok_or("explicit temporary agentchat roots required")?;
        if PathBuf::from(actual).canonicalize()? != root.join(tail).canonicalize()? {
            return Err("probe roots must belong to the owned fixture".into());
        }
    }
    Ok(())
}
pub(super) fn registration(root: &Path, observer: &str) -> Result<PathBuf> {
    if observer.is_empty()
        || observer.len() > 100
        || !observer
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("invalid native observer".into());
    }
    Ok(root.join(format!("lifecycle-{observer}.json")))
}
pub(super) fn audit(root: &Path, event: serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec(&event)?;
    if bytes.len() > 4096 {
        return Err("oversized probe audit event".into());
    }
    let path = root.join("gateway-events.jsonl");
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return Err("unsafe probe audit file".into());
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    f.lock()?;
    if f.metadata()?.len() + bytes.len() as u64 + 1 > 1_048_576 {
        return Err("probe audit bound reached".into());
    }
    f.write_all(&bytes)?;
    f.write_all(b"\n")?;
    Ok(())
}
