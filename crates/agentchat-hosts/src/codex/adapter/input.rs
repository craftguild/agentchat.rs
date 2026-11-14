//! One application-owned FIFO head; the adapter reports native acceptance without writing delivery/read state.
use super::NativeAdapter;
use crate::{
    codex::{Client, required, unavailable},
    launch::Endpoint,
};
use agentchat_core::{
    ErrorCode, Result,
    delivery::{
        MessageDispatch, MessagePort, MessageProgress, NativeInput, NoticeDispatch,
        SUBMISSION_SECONDS,
    },
    model::{Context, HostKind},
    project::NativeFrameRevision,
};
use std::time::Duration;

#[async_trait::async_trait]
impl MessagePort for NativeAdapter {
    async fn present_message(
        &self,
        context: &Context,
        dispatch: &MessageDispatch,
    ) -> Result<MessageProgress> {
        current_revision(dispatch.frame_revision)?;
        tokio::time::timeout(
            Duration::from_secs(SUBMISSION_SECONDS),
            self.input(
                context,
                NativeInput::Message(dispatch.input.clone()),
                dispatch.token,
                dispatch.attempt,
                dispatch.awaiting_acceptance,
            ),
        )
        .await
        .map_err(|_| unknown())?
    }

    async fn present_notice(
        &self,
        context: &Context,
        dispatch: &NoticeDispatch,
    ) -> Result<MessageProgress> {
        current_revision(dispatch.frame_revision)?;
        tokio::time::timeout(
            Duration::from_secs(SUBMISSION_SECONDS),
            self.input(
                context,
                NativeInput::Notice(dispatch.input.clone()),
                dispatch.token,
                dispatch.attempt,
                dispatch.awaiting_acceptance,
            ),
        )
        .await
        .map_err(|_| unknown())?
    }
}

fn current_revision(revision: NativeFrameRevision) -> Result<()> {
    if revision != NativeFrameRevision::DispatchV4 {
        return Err(agentchat_core::Error::new(
            ErrorCode::UnsupportedProtocol,
            "A historical native frame cannot be resubmitted by this adapter.",
        ));
    }
    Ok(())
}
impl NativeAdapter {
    async fn input(
        &self,
        context: &Context,
        input: NativeInput,
        token: uuid::Uuid,
        attempt: u32,
        awaiting_acceptance: bool,
    ) -> Result<MessageProgress> {
        // An unsupported recovery request proves nothing about whether its
        // original input entered a previous adapter. Never report non-entry.
        if awaiting_acceptance {
            return Err(agentchat_core::Error::new(
                ErrorCode::UnsupportedProtocol,
                "This adapter cannot read an original native input receipt.",
            ));
        }
        let mut submitted = false;
        let outcome = self
            .submit(
                context,
                input,
                token,
                attempt,
                awaiting_acceptance,
                &mut submitted,
            )
            .await;
        match outcome {
            Ok(receipt) => Ok(MessageProgress::Accepted(receipt)),
            // Busy here is only the shared server's explicit -32001 overload
            // refusal. Other post-submission errors, including malformed/lost
            // results, leave the application's original uncertainty fence set.
            Err(error) if !submitted || error.code == ErrorCode::Busy => {
                Ok(MessageProgress::NotAccepted(error))
            }
            Err(mut error) => {
                error.retryable = false;
                Err(error)
            }
        }
    }

    async fn submit(
        &self,
        context: &Context,
        input: NativeInput,
        token: uuid::Uuid,
        attempt: u32,
        awaiting_acceptance: bool,
        submitted: &mut bool,
    ) -> Result<agentchat_core::delivery::HostReceipt> {
        // Codex reports acceptance in its original request result. It has no
        // durable publication/receipt lane: an awaiting call must never resend
        // an ambiguous predecessor under the guise of reading its receipt.
        if context.host != HostKind::Codex
            || awaiting_acceptance
            || token.is_nil()
            || !(1..=3).contains(&attempt)
            || context.project_id != input.project_id()
        {
            return Err(required());
        }
        input.text()?;
        let route = self.registry.observe(context)?;
        if route.binding().identity.generation != input.generation() {
            return Err(required());
        }
        let Endpoint::Codex { socket, .. } = &route.binding().endpoint else {
            return Err(required());
        };
        let mut client = Client::connect(socket, &context.host_version).await?;
        let final_scope = async {
            // Initialization and thread/read can yield while the actual owner
            // exits or its context is replaced. Recheck the fixed association
            // immediately before input; native switching never moves it.
            let current = self.registry.observe(context)?;
            if current != route {
                return Err(required());
            }
            Ok(())
        };
        client
            .deliver_native_checked(
                context,
                &route.binding().identity,
                &input,
                final_scope,
                submitted,
            )
            .await
    }
}

fn unknown() -> agentchat_core::Error {
    let mut error = unavailable();
    // A new observation may retry, but this complete-body operation may already
    // have been accepted. Only the application can settle its retained head.
    error.retryable = false;
    error
}
