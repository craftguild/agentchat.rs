//! Read-only socket validation; publication and retirement own separate proof.
use super::unavailable;
pub(super) use super::{publication::bind, recovery::retire};
use crate::claude::observer::files;
use agentchat_core::Result;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};
use tokio::net::UnixStream;

pub(super) async fn socket(path: &Path) -> Result<()> {
    files::directory(path.parent().ok_or_else(unavailable)?).await?;
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| unavailable())?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(unavailable());
    }
    Ok(())
}
pub(super) fn same_user(stream: &UnixStream) -> bool {
    stream.peer_cred().map(|c| c.uid()).ok() == Some(rustix::process::geteuid().as_raw())
}
