//! Use the same strict bounded metadata reader as native registration/dispatch.
use agentchat_core::Result;
use serde::de::DeserializeOwned;
use std::path::Path;
pub(super) async fn directory(path: &Path) -> Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || crate::claude::metadata::private_directory(&path))
        .await
        .map_err(|_| super::unavailable())?
}
pub(super) async fn read<T: DeserializeOwned + Send + 'static>(path: &Path) -> Result<T> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || crate::claude::metadata::read(&path))
        .await
        .map_err(|_| super::unavailable())?
}
