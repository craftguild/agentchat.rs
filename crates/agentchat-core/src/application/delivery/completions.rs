//! Completed native outcomes share durability, never eligibility or identities.
use super::Application;
use crate::{
    Error, ErrorCode, Result,
    application::{CommitOutcome, CommitRequest},
    delivery::{MAX_WAKE_COMPLETION_GROUP, WakeCompletion},
};

impl Application {
    /// Record one through eight already-completed native attempts in input order.
    ///
    /// Each callback retains the ordinary exact-token policy and isolated
    /// savepoint. Nothing returns before the outer FULL commit. Infrastructure
    /// failure aborts provisional results; semantic callback errors stay local.
    /// Recovery uses original claims after an ambiguous commit. No callback
    /// acknowledges deliveries, marks read or renews host presence.
    pub async fn finish_wakes(
        &self,
        completions: Vec<WakeCompletion>,
    ) -> Result<Vec<Result<bool>>> {
        if completions.is_empty() || completions.len() > MAX_WAKE_COMPLETION_GROUP {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Wake completion group must contain one through eight outcomes.",
            ));
        }
        Ok(self
            .commit_ready(
                completions
                    .into_iter()
                    .map(CommitRequest::WakeCompletion)
                    .collect(),
            )
            .await?
            .into_iter()
            .map(|result| {
                result.map(|outcome| match outcome {
                    CommitOutcome::WakeCompletion(recorded) => recorded,
                    CommitOutcome::Mutation(_)
                    | CommitOutcome::MessageCompletion(_)
                    | CommitOutcome::NativeCompletion(_)
                    | CommitOutcome::NativeProcessing(_)
                    | CommitOutcome::NativeClaim(_) => {
                        unreachable!("wake-completion-only commit group")
                    }
                })
            })
            .collect())
    }
}
