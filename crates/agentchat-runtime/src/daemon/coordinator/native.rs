//! Preparation failures are local diagnostics, never fabricated native receipts.

use super::Worker;
use crate::{
    daemon::delivery::{Finished, Outcome},
    diagnostics::Event,
};
use agentchat_core::model::LaunchState;
use std::collections::HashSet;

impl Worker {
    pub(super) async fn reconcile_delivery_contexts(&mut self) {
        let contexts = self.attached.values().cloned().collect::<Vec<_>>();
        let mut seen = HashSet::new();
        for context in contexts {
            if !seen.insert(context.context_id) {
                continue;
            }
            match self.app.launch_state(&context).await {
                Ok(LaunchState::Current) => {}
                Ok(LaunchState::Inactive) => {
                    self.deliveries.cancel(context.context_id);
                    self.attached
                        .retain(|_, attached| attached.context_id != context.context_id);
                }
                Ok(LaunchState::Unconfirmed) | Err(_) => {
                    // Cancellation cannot recall a native side effect. The
                    // durable claim remains fenced; an inconclusive read cannot
                    // remove the attachment or discard pending work.
                    self.deliveries.cancel(context.context_id);
                }
            }
        }
    }
    pub(super) fn take_native_ready(
        &mut self,
        first: Option<Outcome>,
        limit: usize,
    ) -> Vec<Finished> {
        let (finished, failure, completed_job, empty_at) =
            self.deliveries.take_ready_with_empty(first, limit);
        if let Some(head) = empty_at {
            self.native_empty_at = Some(self.native_empty_at.map_or(head, |prior| prior.min(head)));
        }
        self.native_jobs_completed |= completed_job;
        if let Some(error) = failure {
            self.control_error(Event::NativeFailed { code: error.code }, error.code);
        }
        finished
    }
}
