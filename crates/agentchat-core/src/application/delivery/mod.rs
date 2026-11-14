//! Application-owned claim/completion policy; native I/O stays outside SQLite.

mod candidates;
mod check;
mod completions;
mod input;
mod native;
mod native_claim;
pub(super) mod native_preparation;
mod policy;
mod prepare;
pub(super) mod processing;
mod progress;
mod receipt;
pub(super) mod reconcile;
mod stream;

use super::Application;
use crate::{
    Result,
    delivery::{HostReceipt, WakeDispatch},
    model::Context,
};

impl Application {
    /// Reserve one bounded presentation attempt after verifying exact live host
    /// eligibility. Returns only after commit; never invokes native I/O itself.
    /// Callers must finish or time out the attempt outside the write transaction.
    pub async fn prepare_wake(&self, context: &Context) -> Result<Option<WakeDispatch>> {
        self.validate_context(context)?;
        let host = self.host.verify(context).await?;
        let capabilities = self.host.input_capabilities(context)?;
        let mut tx = self.storage.begin().await?;
        self.authorize_participation(tx.as_mut(), context, false)
            .await?;
        let result = prepare::prepare(
            tx.as_mut(),
            context,
            &host,
            self.clock.now(),
            capabilities.active_hint,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Record native acceptance/failure without acknowledgement, read receipts
    /// or heartbeats. Late/repeated/retired callbacks return false, not new work.
    pub async fn finish_wake(
        &self,
        context: &Context,
        dispatch: &WakeDispatch,
        outcome: Result<HostReceipt>,
    ) -> Result<bool> {
        self.finish_wakes(vec![crate::delivery::WakeCompletion {
            context: context.clone(),
            dispatch: dispatch.clone(),
            outcome,
        }])
        .await?
        .pop()
        .expect("one completion result")
    }
}
