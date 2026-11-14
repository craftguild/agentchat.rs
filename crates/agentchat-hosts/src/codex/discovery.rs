//! Read-only shared-daemon location and matching actual native executable.
use super::{invalid, unavailable};
use agentchat_core::{Error, ErrorCode, Result};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};
#[cfg(test)]
#[path = "../../tests/unit/codex_discovery.rs"]
mod tests;

/// Exact installed daemon metadata; not a host admission or conversation proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    /// Canonical existing local shared-server control socket. Native discovery
    /// may publish a stable symlink; later ownership/type checks inspect its
    /// resolved target rather than rejecting that ordinary indirection.
    pub socket: PathBuf,
    /// Real native executable reported by that daemon, never the npm shim.
    pub executable: PathBuf,
    /// Matching actual managed executable/server version.
    pub version: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Probe {
    status: String,
    managed_codex_path: PathBuf,
    managed_codex_version: String,
    socket_path: PathBuf,
    app_server_version: String,
}
impl Daemon {
    /// Inspect only an existing daemon. No start/restart/update, config override,
    /// authentication copy, model call or second server is allowed here.
    pub async fn inspect(cli: &Path) -> Result<Self> {
        let work = async {
            let mut daemon =
                Self::decode(&output(cli, &["app-server", "daemon", "version"]).await?)?;
            daemon.socket = canonical_socket(&daemon.socket)?;
            let actual = output(&daemon.executable, &["--version"]).await?;
            if std::str::from_utf8(&actual).ok().map(str::trim)
                != Some(format!("codex-cli {}", daemon.version).as_str())
            {
                return Err(Error::new(
                    ErrorCode::UnsupportedHost,
                    "The actual native Codex executable version differs from its shared server.",
                ));
            }
            Ok(daemon)
        };
        timeout(Duration::from_secs(5), work)
            .await
            .map_err(|_| unavailable())?
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        let probe: Probe = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if probe.status != "running"
            || !probe.socket_path.is_absolute()
            || !probe.managed_codex_path.is_absolute()
            || probe.app_server_version.is_empty()
            || probe.app_server_version.len() > 128
            || probe.app_server_version != probe.managed_codex_version
        {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "A running shared Codex server and matching native CLI version are required.",
            ));
        }
        // The inspection command can be an older PATH shim. It only locates
        // the existing daemon; inspect() independently executes and verifies
        // the reported real native image before that image can be selected.
        let executable = probe
            .managed_codex_path
            .canonicalize()
            .map_err(|_| unavailable())?;
        crate::launch::native_executable(&executable)?;
        Ok(Self {
            socket: probe.socket_path,
            executable,
            version: probe.app_server_version,
        })
    }
}

fn canonical_socket(path: &Path) -> Result<PathBuf> {
    path.canonicalize().map_err(|_| unavailable())
}

pub(super) async fn output(program: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| unavailable())?;
    let mut output = Vec::new();
    let mut stdout = child.stdout.take().ok_or_else(unavailable)?.take(16_385);
    let (read, status) = tokio::join!(stdout.read_to_end(&mut output), child.wait());
    read.map_err(|_| unavailable())?;
    if !status.map_err(|_| unavailable())?.success() || output.len() > 16_384 {
        return Err(unavailable());
    }
    Ok(output)
}
