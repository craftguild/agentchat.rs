//! Bounded native-image discovery; selection is never live launch authority.
use crate::claude::{module, observer::NativeProfile};
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    ffi::OsString,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

#[cfg(test)]
#[path = "../../tests/unit/claude_profile.rs"]
mod tests;

pub(super) async fn discover(search: Option<OsString>) -> Result<NativeProfile> {
    let executable = locate(search)?;
    crate::launch::native_executable(&executable)?;
    let before = stamp(&executable)?;
    let version = decode(&output(&executable, &["--version"]).await?)?;
    if stamp(&executable)? != before {
        return Err(unavailable());
    }
    Ok(NativeProfile {
        executable,
        version,
    })
}

fn locate(search: Option<OsString>) -> Result<PathBuf> {
    let path = super::program::locate("claude", search)?;
    super::encoded(&path)?;
    Ok(path)
}

fn stamp(path: &Path) -> Result<(u64, u64, u64, i64, i64, u32)> {
    let meta = std::fs::metadata(path).map_err(|_| unavailable())?;
    Ok((
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.mode(),
    ))
}

fn decode(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| unavailable())?
        .trim();
    let version = text
        .strip_suffix(" (Claude Code)")
        .ok_or_else(unavailable)?;
    if !module::candidate_version(version) {
        return Err(unavailable());
    }
    Ok(version.into())
}

async fn output(program: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| unavailable())?;
    let mut stdout = child.stdout.take().ok_or_else(unavailable)?.take(4097);
    let mut bytes = Vec::new();
    let result = timeout(Duration::from_secs(2), async {
        tokio::join!(stdout.read_to_end(&mut bytes), child.wait())
    })
    .await;
    let (read, status) = match result {
        Ok(result) => result,
        Err(_) => {
            // The version probe is our own direct child. Reap it explicitly;
            // a failed observation cannot leave a background process behind.
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(unavailable());
        }
    };
    read.map_err(|_| unavailable())?;
    if !status.map_err(|_| unavailable())?.success() || bytes.len() > 4096 {
        return Err(unavailable());
    }
    Ok(bytes)
}

fn unavailable() -> Error {
    Error::new(
        ErrorCode::UnsupportedHost,
        "An unchanged native Claude executable and bounded version observation are required.",
    )
}
