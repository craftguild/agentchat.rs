//! Receipt-first initialization retry without weakening fresh routing checks.

use super::{
    InitOutcome, InitRequest, ProjectManager, blocking, discovery, journal, metadata, retirement,
};
use agentchat_core::Result;
use std::path::Path;

impl ProjectManager {
    /// Resume only an already recorded exact initialization request.
    /// This permits a successful --new-project retry after its own old context
    /// becomes obsolete. Missing request keys return none without creating files;
    /// fresh requests still require the caller's ordinary routing validation.
    pub async fn resume_initialization(
        &self,
        path: impl AsRef<Path>,
        request: InitRequest,
    ) -> Result<Option<InitOutcome>> {
        let mut request = request;
        request.normalize()?;
        let directory = path.as_ref().to_owned();
        let manager = self.clone();
        let mut value = request.clone();
        value.expected_project_id = None;
        let original = directory.clone();
        let recorded = blocking(move || {
            retirement::check(&manager, value.request_id)?;
            let normalized = journal::Normalized {
                cwd_bytes: metadata::native(&discovery::canonical(&directory)?),
                request: value,
            };
            if let Some(completed) = journal::completed(&manager, normalized.request.request_id)? {
                if completed.request != normalized {
                    return Err(journal::idempotency_conflict());
                }
                return Ok(true);
            }
            if let Some(pending) = journal::pending(&manager)?
                && pending.request.request.request_id == normalized.request.request_id
            {
                if pending.request != normalized {
                    return Err(journal::idempotency_conflict());
                }
                return Ok(true);
            }
            Ok(false)
        })
        .await?;
        if recorded {
            Ok(Some(self.initialize(original, request).await?))
        } else {
            Ok(None)
        }
    }
}
