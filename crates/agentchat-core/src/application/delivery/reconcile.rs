//! Bounded hint settlement and exact-attempt callback fencing.

use super::policy;
use crate::{
    Error, ErrorCode, Result, delivery::*, model::Context, ports::Transaction, time::Timestamp,
};

pub(in crate::application) enum CompletionError {
    // Invalid host receipt data is local to that completed callback. Preserve
    // its ordinary public error code without confusing it with corrupt storage.
    Outcome(Error),
    Application(Error),
}
impl CompletionError {
    pub(in crate::application) fn infrastructure(&self) -> bool {
        matches!(self, Self::Application(error) if matches!(error.code,
            ErrorCode::StorageError | ErrorCode::IoError | ErrorCode::IntegrityError))
    }
    pub(in crate::application) fn into_error(self) -> Error {
        match self {
            Self::Outcome(error) | Self::Application(error) => error,
        }
    }
}
impl From<Error> for CompletionError {
    fn from(error: Error) -> Self {
        Self::Application(error)
    }
}

pub(super) async fn settled_batch(tx: &mut dyn Transaction, batch: &WakeBatch) -> Result<bool> {
    // Callers already loaded this control row in the same immediate writer
    // transaction. Reuse it without caching across requests or duplicating the
    // state transition. Emptiness needs validated deliveries, not event bodies.
    if !batch.state.outstanding() {
        return Ok(batch.state == BatchState::Settled);
    }
    if !tx
        .wake_batch_has_pending(batch.recipient_id, batch.id)
        .await?
    {
        let mut batch = batch.clone();
        batch.state = BatchState::Settled;
        batch.next_eligible_at = None;
        batch.failure_code = None;
        tx.save_wake_batch(&batch).await?;
        return Ok(true);
    }
    Ok(false)
}
pub(in crate::application) async fn finish(
    tx: &mut dyn Transaction,
    context: &Context,
    dispatch: &WakeDispatch,
    outcome: Result<HostReceipt>,
    now: Timestamp,
) -> std::result::Result<bool, CompletionError> {
    let Some(agent) = tx
        .conversation(context.host, &context.host_session_id)
        .await?
        .filter(|a| a.joined)
    else {
        return Ok(false);
    };
    let Some(session) = tx.session(agent.id).await? else {
        return Ok(false);
    };
    if session.incarnation != context.incarnation
        || session.context_id != context.context_id
        || dispatch.hint.context_id != context.context_id
        || dispatch.hint.incarnation != context.incarnation
        || dispatch.hint.project_id != context.project_id
        || dispatch.hint.agent_id != agent.id
    {
        return Ok(false);
    }
    let Some(mut batch) = tx.wake_batch(dispatch.hint.batch_id).await? else {
        return Ok(false);
    };
    let references = dispatch
        .hint
        .events
        .iter()
        .map(|event| event.delivery_id)
        .collect::<Vec<_>>();
    if batch.recipient_id != agent.id
        || batch.incarnation != context.incarnation
        || batch.state != BatchState::Pending
        || batch.dispatch_ref != Some(dispatch.token)
        || batch.attempts != dispatch.attempt
        || policy::dispatch_token(&batch, &references) != dispatch.token
    {
        return Ok(false);
    }
    if settled_batch(tx, &batch).await? {
        return Ok(false);
    }
    match outcome {
        Ok(receipt) => {
            policy::valid_turn(&receipt.turn_id).map_err(CompletionError::Outcome)?;
            if receipt.turn_id.is_some() {
                batch.turn_ref = receipt.turn_id;
            }
            batch.failure_code = None;
            batch.state = BatchState::Inflight;
        }
        Err(error) => {
            batch.failure_code = Some(
                serde_json::to_value(error.code)
                    .expect("error code serialization")
                    .as_str()
                    .expect("string error code")
                    .into(),
            );
            batch.state = if batch.attempts == 3 {
                BatchState::Stalled
            } else {
                BatchState::Inflight
            };
        }
    }
    // Successful third submissions retain a five-second acknowledgement grace
    // and still wait for their associated active turn before becoming stalled.
    batch.next_eligible_at = if batch.state == BatchState::Stalled {
        None
    } else {
        Some(now.after_seconds(if batch.attempts == 1 { 1 } else { 5 })?)
    };
    tx.save_wake_batch(&batch).await?;
    Ok(true)
}
