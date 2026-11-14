//! A nonce relay is transport only; the native answer and kernel owner authorize.
use super::relay::Endpoint as Relay;
use super::{files, unavailable};
use crate::claude::freshness::{Challenge, Snapshot};
use agentchat_core::Result;
use agentchat_protocol::framing;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    time::Duration,
};
use tokio::{io::AsyncReadExt, net::UnixStream};

/// Obtain one bounded answer from the exact native observer's private relay.
/// The caller must own/consume the corresponding Freshness request and recheck
/// the current lifecycle and immutable kernel owner after this external wait.
/// This function alone establishes neither identity nor wake eligibility.
pub async fn exchange(
    root: &Path,
    observer: &str,
    owner_pid: u32,
    request: &Challenge,
) -> Result<Snapshot> {
    if !super::identifier(observer) {
        return Err(unavailable());
    }
    files::directory(root).await?;
    let relay: Relay = files::read(&root.join(format!("fresh-{observer}.json"))).await?;
    if relay.observer != observer || relay.pid == owner_pid || !relay.socket.is_absolute() {
        return Err(unavailable());
    }
    let parent = relay.socket.parent().ok_or_else(unavailable)?;
    files::directory(parent).await?;
    let m = tokio::fs::symlink_metadata(&relay.socket)
        .await
        .map_err(|_| unavailable())?;
    if !m.file_type().is_socket()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
    {
        return Err(unavailable());
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = UnixStream::connect(relay.socket)
            .await
            .map_err(|_| unavailable())?;
        framing::write(&mut stream, request)
            .await
            .map_err(|_| unavailable())?;
        let mut bytes = Vec::new();
        stream
            .take(8193)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| unavailable())?;
        if bytes.len() > 8192 || !bytes.ends_with(b"\n") {
            return Err(unavailable());
        }
        serde_json::from_slice(&bytes).map_err(|_| unavailable())
    })
    .await
    .map_err(|_| unavailable())?
}
