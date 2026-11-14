//! Private native recovery and actor-free diagnostic composition.

use super::{Application, binding, viewer};
use crate::{
    Result,
    model::Context,
    read::{BindingView, HealthView, Snapshot},
};

impl Application {
    /// Inspect the exact native binding for integration recovery without Join.
    /// This helper is distinct from the actor-required public Whoami operation.
    pub async fn native_binding(&self, context: &Context) -> Result<BindingView> {
        self.validate_context(context)?;
        let mut snapshot = self.storage.snapshot().await?;
        let agent = viewer(snapshot.as_mut(), Some(context)).await?;
        binding(snapshot.as_mut(), Some(context), agent, self.clock.now()).await
    }

    /// Compose internal project diagnostics without selecting or creating an actor.
    /// Transports expose this through Doctor and private native control only.
    pub async fn health(&self) -> Result<HealthView> {
        let mut snapshot = self.storage.snapshot().await?;
        health_view(snapshot.as_mut(), self.project_id()).await
    }
}

pub(super) async fn health_view(
    db: &mut dyn Snapshot,
    project_id: uuid::Uuid,
) -> Result<HealthView> {
    Ok(HealthView {
        project_id,
        head: db.head(),
        channel_policy: db
            .channel_policy()
            .await?
            .unwrap_or_else(crate::project::ProjectChannelPolicy::legacy),
        receipt_settings: db.receipt_settings().await?,
        statistics: db.statistics().await?,
    })
}
