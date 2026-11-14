//! Full-input final checks and bounded native I/O, without holding a writer.
use super::{Finished, unavailable};
use crate::diagnostics::{Event, NativeOutcome, NativePhase, Sink};
use agentchat_core::{
    Error, ErrorCode, Result,
    application::Application,
    delivery::{
        MessageCompletion, MessagePort, MessageProgress, NativeCompletion, NativeDispatch,
        SUBMISSION_SECONDS,
    },
    model::Context,
};
use std::time::{Duration, Instant};
use tokio::time::timeout;

pub(super) async fn message(
    app: &Application,
    port: &dyn MessagePort,
    context: Context,
    diagnostics: &Sink,
) -> Result<Option<Finished>> {
    let Some(dispatch) = timeout(
        Duration::from_secs(SUBMISSION_SECONDS),
        app.prepare_message(&context),
    )
    .await
    .unwrap_or_else(|_| Err(unavailable()))?
    else {
        return Ok(None);
    };
    diagnostics.record(Event::NativeInput {
        delivery_id: dispatch.input.delivery_id,
        seq: dispatch.input.seq,
        phase: NativePhase::DispatchStarted,
        receipt_only: dispatch.awaiting_acceptance,
        duration_us: 0,
        outcome: None,
    });
    let mut native_started = None;
    // Recheck the committed head just before handing it to the native port.
    // Its final root/control boundary must independently check this same claim;
    // no writer is held while native work or a hook waits for consumption.
    let outcome = timeout(Duration::from_secs(SUBMISSION_SECONDS), async {
        if !dispatch.awaiting_acceptance {
            let current = app
                .message_is_current(
                    &context,
                    dispatch.input.generation,
                    dispatch.token,
                    dispatch.input.delivery_id,
                )
                .await;
            if !matches!(current, Ok(true)) {
                return Ok(MessageProgress::NotAccepted(current.err().unwrap_or_else(
                    || {
                        Error::new(
                            ErrorCode::SessionRequired,
                            "The committed native message claim is no longer current.",
                        )
                    },
                )));
            }
        }
        // An awaiting dispatch may only poll its original receipt. It emits no
        // native input, so later pause/approval cannot hide a known acceptance.
        // Completion still checks the exact original launch/head/token; unknown
        // or lost owners never obtain this poll from application preparation.
        native_started = Some(Instant::now());
        port.present_message(&context, &dispatch).await
    })
    .await
    .unwrap_or_else(|_| Err(unavailable()));
    diagnostics.record(Event::NativeInput {
        delivery_id: dispatch.input.delivery_id,
        seq: dispatch.input.seq,
        phase: NativePhase::NativeResult,
        receipt_only: dispatch.awaiting_acceptance,
        duration_us: native_started.map_or(0, elapsed),
        outcome: Some(classify(&outcome)),
    });
    Ok(Some(Finished::Message(MessageCompletion {
        context,
        dispatch,
        outcome,
    })))
}

/// Runtime-private preparation boundary; the owner supplies its shared ingress.
#[async_trait::async_trait]
pub(crate) trait Preparation: Send + Sync {
    async fn prepare(&self, context: &Context) -> Result<Option<NativeDispatch>>;
}

pub(super) async fn native(
    app: &Application,
    pipeline: &dyn Preparation,
    port: &dyn MessagePort,
    context: Context,
    diagnostics: &Sink,
) -> Result<Option<Finished>> {
    let Some(dispatch) = pipeline.prepare(&context).await? else {
        return Ok(None);
    };
    diagnostics.record(Event::NativeInput {
        delivery_id: dispatch.delivery_id(),
        seq: dispatch.seq(),
        phase: NativePhase::DispatchStarted,
        receipt_only: dispatch.awaiting_acceptance(),
        duration_us: 0,
        outcome: None,
    });
    let mut native_started = None;
    let outcome = timeout(Duration::from_secs(SUBMISSION_SECONDS), async {
        if !dispatch.awaiting_acceptance() {
            let current = app
                .native_is_current(
                    &context,
                    dispatch.generation(),
                    dispatch.token(),
                    dispatch.delivery_id(),
                )
                .await;
            if !matches!(current, Ok(true)) {
                return Ok(MessageProgress::NotAccepted(current.err().unwrap_or_else(
                    || {
                        Error::new(
                            ErrorCode::SessionRequired,
                            "The committed native input claim is no longer current.",
                        )
                    },
                )));
            }
        }
        native_started = Some(Instant::now());
        match &dispatch {
            NativeDispatch::Message(message) => port.present_message(&context, message).await,
            NativeDispatch::Notice(notice) => port.present_notice(&context, notice).await,
        }
    })
    .await
    .unwrap_or_else(|_| Err(unavailable()));
    diagnostics.record(Event::NativeInput {
        delivery_id: dispatch.delivery_id(),
        seq: dispatch.seq(),
        phase: NativePhase::NativeResult,
        receipt_only: dispatch.awaiting_acceptance(),
        duration_us: native_started.map_or(0, elapsed),
        outcome: Some(classify(&outcome)),
    });
    Ok(Some(Finished::Native(NativeCompletion {
        context,
        dispatch,
        outcome,
    })))
}
fn classify(result: &Result<MessageProgress>) -> NativeOutcome {
    match result {
        Ok(MessageProgress::Accepted(_)) => NativeOutcome::Accepted,
        Ok(MessageProgress::AwaitingAcceptance) => NativeOutcome::AwaitingAcceptance,
        Ok(MessageProgress::NotAccepted(_)) => NativeOutcome::NotAccepted,
        Err(_) => NativeOutcome::Unknown,
    }
}
fn elapsed(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}
