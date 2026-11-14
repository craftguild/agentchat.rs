//! Shared native body encoding, with a final fixed-launch check for the port.
use super::{Client, invalid, required, thread_id};
use crate::launch::LaunchOwners;
use agentchat_core::{
    Result,
    delivery::{HostReceipt, MessageInput, NativeInput},
    model::{Context, HostKind, LaunchIdentity, LaunchOwnerState, Phase},
};
use serde_json::{Value, json};
use std::future::Future;

impl Client {
    /// Submit one complete attributed body under original kernel launch proof.
    /// This low-level protocol component does not resolve a registered launch;
    /// product callers use NativeAdapter's MessagePort and its final scope gate.
    /// The application owns frozen recipient selection and durable FIFO claims.
    /// Native acceptance never ACKs, reads, renews authority or waits for a model.
    pub async fn deliver(
        &mut self,
        context: &Context,
        identity: &LaunchIdentity,
        input: &MessageInput,
    ) -> Result<HostReceipt> {
        self.deliver_checked(context, identity, input, std::future::ready(Ok(())))
            .await
    }

    /// Gate both routes after metadata I/O with the caller's fresh original
    /// launch/owner proof. The guard never replaces native controls or ownership.
    pub(crate) async fn deliver_checked(
        &mut self,
        context: &Context,
        identity: &LaunchIdentity,
        input: &MessageInput,
        final_scope: impl Future<Output = Result<()>>,
    ) -> Result<HostReceipt> {
        let mut submitted = false;
        self.deliver_native_checked(
            context,
            identity,
            &NativeInput::Message(input.clone()),
            final_scope,
            &mut submitted,
        )
        .await
    }

    /// Submit either canonical input kind through the same native routes and
    /// exact final fixed-launch/kernel proof.
    pub(crate) async fn deliver_native_checked(
        &mut self,
        context: &Context,
        identity: &LaunchIdentity,
        input: &NativeInput,
        final_scope: impl Future<Output = Result<()>>,
        submitted: &mut bool,
    ) -> Result<HostReceipt> {
        if context.host != HostKind::Codex
            || context.host_version != self.version
            || context.project_id != input.project_id()
            || input.generation() != identity.generation
            || !thread_id(&context.host_session_id)
            || LaunchOwners::observe(identity) != LaunchOwnerState::Current
        {
            return Err(required());
        }
        let text = input.text()?;
        let phase = self.observe(&context.host_session_id).await?;
        if !matches!(phase.phase, Phase::Idle | Phase::Active) || phase.wake_paused {
            return Err(required());
        }
        final_scope.await?;
        if LaunchOwners::observe(identity) != LaunchOwnerState::Current {
            return Err(required());
        }
        // From this point even a lost reply/cancelled socket can follow native
        // acceptance. The caller may classify only pre-submission errors or an
        // explicit overloaded-request refusal as safe negative outcomes.
        *submitted = true;
        if input.addressed() {
            let result = self.request("turn/start", json!({"threadId":context.host_session_id,"input":[{"type":"text","text":text}]})).await?;
            let turn = self.decoded(
                "turn/start.result.turn.id",
                result
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
                    .ok_or_else(invalid),
            )?;
            // Native acceptance describes an input already taken. A later
            // control/root change cannot downgrade it and authorize a resend;
            // the application independently fences its original head at commit.
            Ok(HostReceipt {
                turn_id: Some(turn.into()),
            })
        } else {
            self.inject(&context.host_session_id, &text).await?;
            Ok(HostReceipt::default())
        }
    }
}
