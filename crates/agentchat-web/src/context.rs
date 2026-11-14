//! Immutable display metadata injected by executable composition, not domain reads.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Human-readable launch directory; never a routing key or conversation identity.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerContext {
    /// Final directory component only, without exposing a user home or full path.
    pub directory_name: Option<String>,
}
impl ViewerContext {
    /// Name an already resolved launch directory using the platform's Path
    /// semantics, without filesystem access. Filesystem roots use `Root`.
    pub fn from_directory(directory: &Path) -> Self {
        let label = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Root".to_owned());
        Self {
            directory_name: Some(label),
        }
    }
}
