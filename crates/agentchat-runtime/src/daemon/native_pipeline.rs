//! Verified owner-side composition of shared Core native preparation stages.
use super::coordinator::{NativeIngress, NativeRequest};
use agentchat_core::{
    Result,
    application::{Application, CommitOutcome, NativePreparation},
    delivery::{MessageProgress, NativeCompletion, NativeDispatch, SUBMISSION_SECONDS},
    model::Context,
};
use std::{sync::Arc, time::Duration};

/// Native writes for one verified Server, using its existing bounded ingress.
/// No constructor accepts a caller-supplied application, project or authority.
#[derive(Clone)]
pub struct NativePipeline {
    app: Arc<Application>,
    ingress: NativeIngress,
}
impl NativePipeline {
    pub(super) fn new(app: Arc<Application>, ingress: NativeIngress) -> Self {
        Self { app, ingress }
    }
    /// Keep the original single five-second prepare budget across all stages.
    pub async fn prepare_native(&self, context: &Context) -> Result<Option<NativeDispatch>> {
        let duration = Duration::from_secs(SUBMISSION_SECONDS);
        let deadline = tokio::time::Instant::now() + duration;
        tokio::time::timeout(duration, self.prepare(context, deadline))
            .await
            .unwrap_or_else(|_| {
                Err(super::unavailable(
                    "Native preparation exceeded its bounded deadline.",
                ))
            })
    }
    async fn prepare(
        &self,
        context: &Context,
        deadline: tokio::time::Instant,
    ) -> Result<Option<NativeDispatch>> {
        let (request, _caller) = self
            .app
            .native_processing(context)?
            .until(deadline.into_std());
        let processing = match self
            .ingress
            .write(NativeRequest::Processing(request), Some(deadline))
            .await?
        {
            CommitOutcome::NativeProcessing(value) => value,
            _ => return Err(lost()),
        };
        // Heavy host observation and original receipt recovery never run in
        // the writer coordinator or before Processing durability is returned.
        match self.app.observe_native(processing).await? {
            NativePreparation::Original(value) => Ok(value),
            NativePreparation::Claim(request) => match self
                .ingress
                .write(NativeRequest::Claim(request), Some(deadline))
                .await?
            {
                CommitOutcome::NativeClaim(value) => Ok(value.into_dispatch()),
                _ => Err(lost()),
            },
        }
    }
    /// Retain an actual original native result even if its caller later leaves.
    /// Core still validates its exact original owner/root/generation/head/token.
    pub async fn finish_native(
        &self,
        context: &Context,
        dispatch: &NativeDispatch,
        outcome: Result<MessageProgress>,
    ) -> Result<bool> {
        match self
            .ingress
            .write(
                NativeRequest::Completion(Box::new(NativeCompletion {
                    context: context.clone(),
                    dispatch: dispatch.clone(),
                    outcome,
                })),
                None,
            )
            .await?
        {
            CommitOutcome::NativeCompletion(value) => Ok(value),
            _ => Err(lost()),
        }
    }
}
fn lost() -> agentchat_core::Error {
    agentchat_core::Error::new(
        agentchat_core::ErrorCode::IntegrityError,
        "Native ready write returned the wrong outcome.",
    )
}

#[async_trait::async_trait]
impl super::delivery::input::Preparation for NativePipeline {
    async fn prepare(&self, context: &Context) -> Result<Option<NativeDispatch>> {
        self.prepare_native(context).await
    }
}
