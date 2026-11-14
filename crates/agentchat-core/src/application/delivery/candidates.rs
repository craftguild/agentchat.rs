//! Read-only pruning is advisory; every positive still needs ordinary preparation.

use super::Application;
use crate::{Error, ErrorCode, Result, delivery::MAX_WAKE_SCAN_CONTEXTS, model::Context};
use std::collections::BTreeSet;
use uuid::Uuid;

impl Application {
    /// Read a fresh conservative hint without host verification or a writer.
    /// Invalid descriptors remain candidates so their ordinary per-recipient
    /// errors cannot suppress independent recipients. A hint error/timeout must
    /// fall back to preparing every input; this method never grants a claim.
    pub async fn wake_candidates(&self, contexts: &[Context]) -> Result<BTreeSet<Uuid>> {
        if contexts.len() > MAX_WAKE_SCAN_CONTEXTS {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Wake candidate scan exceeds 32 contexts.",
            ));
        }
        let (valid, invalid): (Vec<_>, Vec<_>) = contexts
            .iter()
            .cloned()
            .partition(|context| self.validate_context(context).is_ok());
        let mut ids = invalid
            .iter()
            .map(|c| c.context_id)
            .collect::<BTreeSet<_>>();
        if !valid.is_empty() {
            let selected = self
                .storage
                .wake_candidates(&valid, self.clock.now())
                .await?;
            if selected.len() > MAX_WAKE_SCAN_CONTEXTS
                || selected
                    .iter()
                    .any(|id| !valid.iter().any(|c| c.context_id == *id))
            {
                return Err(Error::new(
                    ErrorCode::IntegrityError,
                    "Wake candidate scan returned unrelated contexts.",
                ));
            }
            ids.extend(selected);
        }
        Ok(ids)
    }
}
