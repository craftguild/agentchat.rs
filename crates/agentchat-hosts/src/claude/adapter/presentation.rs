//! File publication follows native proof; Core owns receipt settlement and reads.
use super::{NativeAdapter, required};
use crate::claude::{
    metadata, routing, signal,
    wake::{Submission, WakeRecord},
};
use agentchat_core::{
    Result,
    delivery::{HostReceipt, MAX_WAKE_BYTES, MAX_WAKE_EVENTS, Presentation, WakeDispatch},
    model::{Context, Phase},
};
use std::time::{Duration, Instant};
impl NativeAdapter {
    /// Submit one bounded exact-context hint, then wait for its matching native
    /// static-command acceptance. Callers must already hold a durable application
    /// claim and independently select/admit the exact host profile. Acceptance
    /// never acknowledges delivery, marks reading or proves model receipt.
    pub async fn present(&self, context: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        tokio::time::timeout(Duration::from_secs(5), self.submit(context, dispatch))
            .await
            .map_err(|_| required())?
    }
    async fn submit(&self, context: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        if dispatch.token.is_nil()
            || !(1..=3).contains(&dispatch.attempt)
            || dispatch.hint.version != 1
            || dispatch.hint.batch_id.is_nil()
            || dispatch.hint.agent_id.is_nil()
            || dispatch.hint.events.is_empty()
            || dispatch.hint.events.len() > MAX_WAKE_EVENTS
            || dispatch.hint.preview.len() > 512
            || dispatch.hint.events.iter().any(|e| {
                !matches!(
                    e.kind.as_str(),
                    "message.created" | "message.delivery_confirmation_timeout"
                )
            })
            || serde_json::to_vec(&dispatch.hint)
                .map_err(|_| required())?
                .len()
                > MAX_WAKE_BYTES
            || dispatch.hint.context_id != context.context_id
            || dispatch.hint.incarnation != context.incarnation
            || dispatch.hint.project_id != context.project_id
        {
            return Err(required());
        }
        let route = routing::resolve(&self.home, context).await?;
        let state = self.observe_route(&route).await?;
        let expected = match &dispatch.presentation {
            Presentation::Idle => None,
            Presentation::Active { expected_turn_id } => expected_turn_id.clone(),
        };
        if state.wake_paused
            || !match &dispatch.presentation {
                Presentation::Idle => state.phase == Phase::Idle,
                Presentation::Active { expected_turn_id } => {
                    state.phase == Phase::Active && &state.active_turn_id == expected_turn_id
                }
            }
        {
            return Err(required());
        }
        let home = self.home.clone();
        let cwd = context.working_directory.clone();
        let ns = route.namespace.clone();
        let publisher = ns.metadata_publisher()?;
        let reservation = route.reservation();
        let path = ns.directory().to_owned();
        let native = route.native.registration.clone();
        let presentation = dispatch.presentation.clone();
        let dispatch = dispatch.clone();
        let token = dispatch.token;
        let generation = ns
            .publish_current(reservation, move |_| {
                signal::send(&home, &cwd, |generation| {
                    publisher.write(
                        &path.join(format!("wake-{}.json", native.context.context_id)),
                        &WakeRecord {
                            root: native.root,
                            context_id: native.context.context_id,
                            incarnation: native.context.incarnation,
                            generation,
                            token: dispatch.token,
                            expected_turn_id: expected,
                            hint: dispatch.hint,
                        },
                    )?;
                    Ok(generation)
                })
            })
            .await?;
        let receipt_path = route
            .namespace
            .directory()
            .join(format!("submitted-{}.json", context.context_id));
        let started = Instant::now();
        loop {
            route.check().await?;
            let path = receipt_path.clone();
            let receipt =
                tokio::task::spawn_blocking(move || match std::fs::symlink_metadata(&path) {
                    Ok(_) => metadata::read::<Submission>(&path).map(Some),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(_) => Err(required()),
                })
                .await
                .map_err(|_| required())??;
            if let Some(receipt) = receipt
                && receipt.accepted
                && receipt.token == token
                && receipt.context_id == context.context_id
                && receipt.incarnation == context.incarnation
                && receipt.root == route.native.registration.root
                && receipt.generation == generation
            {
                route.check().await?;
                return Ok(HostReceipt {
                    turn_id: match &presentation {
                        Presentation::Active { expected_turn_id } => expected_turn_id.clone(),
                        Presentation::Idle => None,
                    },
                });
            }
            if started.elapsed() >= Duration::from_secs(5) {
                return Err(required());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
