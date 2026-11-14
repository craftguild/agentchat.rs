//! Preserving bounded merge of Git-local exclusions, with existing file modes.

use super::fileio;
use agentchat_core::Result;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

pub(super) fn merge(path: &Path) -> Result<()> {
    let common = path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| fileio::conflict("Git exclusion path has no common directory."))?;
    if common.canonicalize().map_err(fileio::io)? != common {
        return Err(fileio::conflict(
            "Git common directory moved during initialization.",
        ));
    }
    let old = fileio::bytes(path, 1024 * 1024)?.unwrap_or_default();
    let mode = match fs::metadata(path) {
        Ok(meta) => meta.permissions().mode() & 0o777,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0o600,
        Err(error) => return Err(fileio::io(error)),
    };
    let patterns = [
        b"**/.agentchat/project.json".as_slice(),
        b".agentchat-wakeup",
        b"**/.agentchat/.agentchat-stage-*",
    ];
    let missing = patterns
        .into_iter()
        .filter(|p| {
            !old.split(|b| *b == b'\n')
                .any(|l| l.strip_suffix(b"\r").unwrap_or(l) == *p)
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return fileio::durable(path);
    }
    let newline = if old.windows(2).any(|p| p == b"\r\n") {
        b"\r\n".as_slice()
    } else {
        b"\n".as_slice()
    };
    let mut bytes = old.clone();
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bytes.extend_from_slice(newline);
    }
    bytes.extend_from_slice(b"# AgentChat project-local metadata");
    bytes.extend_from_slice(newline);
    for pattern in missing {
        bytes.extend_from_slice(pattern);
        bytes.extend_from_slice(newline);
    }
    if bytes.len() > 1024 * 1024 {
        return Err(fileio::conflict(
            "Merged Git exclusions exceed their bounded size.",
        ));
    }
    if fileio::bytes(path, 1024 * 1024)?.unwrap_or_default() != old {
        return Err(fileio::conflict(
            "Git exclusions changed during the preserving merge.",
        ));
    }
    fileio::atomic(path, &bytes, mode)
}
