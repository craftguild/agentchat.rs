//! Exact read-only binding and native progress at one snapshot boundary.

use super::*;

pub(super) async fn binding(
    db: &mut dyn Snapshot,
    context: Option<&Context>,
    agent: Option<Agent>,
    now: Timestamp,
) -> Result<BindingView> {
    let context = context.ok_or_else(|| {
        Error::new(
            ErrorCode::SessionRequired,
            "Whoami requires an exact host descriptor.",
        )
    })?;
    let participation = agent.as_ref().map_or(Participation::Unjoined, |a| {
        if a.joined {
            Participation::Joined
        } else {
            Participation::Left
        }
    });
    let native_delivery = if let Some(agent) = &agent {
        if let Some(launch) = db.latest_launch(agent.id).await? {
            db.message_stream(launch.identity.generation)
                .await?
                .map(|s| {
                    let state = if s.cancelled {
                        NativeDeliveryState::Cancelled
                    } else if s.head.as_ref().is_some_and(|h| h.acceptance_unknown(now)) {
                        NativeDeliveryState::AcceptanceUnknown
                    } else if s.head.as_ref().is_some_and(|h| {
                        !h.awaiting_acceptance
                            && h.attempts == 3
                            && (h.failure_code.is_some() || now >= h.retry_at)
                    }) {
                        NativeDeliveryState::Stalled
                    } else if s.head.is_some() {
                        NativeDeliveryState::Pending
                    } else {
                        NativeDeliveryState::Ready
                    };
                    NativeDeliveryView {
                        generation: s.generation,
                        settled_seq: s.settled_seq,
                        state,
                        head: s.head.map(|h| NativeHeadView {
                            delivery_id: h.delivery_id,
                            receipt_policy: h.receipt_policy,
                            seq: h.seq,
                            attempts: h.attempts,
                            next_eligible_at: h.retry_at,
                            failure_code: h.failure_code,
                            awaiting_acceptance: h.awaiting_acceptance,
                            submission_pending: h.submission_pending,
                        }),
                    }
                })
        } else {
            None
        }
    } else {
        None
    };
    let view = if let Some(agent) = agent {
        Some(agent_view(db, agent, now).await?)
    } else {
        None
    };
    Ok(BindingView {
        project_id: context.project_id,
        context_id: context.context_id,
        participation,
        agent: view,
        native_delivery,
    })
}
