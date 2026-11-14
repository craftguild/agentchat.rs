//! Shared application operations, atomic results and semantic retry identity.

mod actions;
mod activation;
mod actors;
mod command;
mod commits;
mod confirmations;
mod delivery;
mod group;
mod launch;
mod lifecycle;
mod managed_activation;
mod message_mentions;
mod messages;
mod mutation;
mod participation;
mod queries;
mod reservations;
mod work;

pub use command::Mutation;
pub use commits::{CommitOutcome, CommitRequest, MAX_COMMIT_GROUP};
pub use delivery::native_preparation::{
    ClaimedNative, NativeClaim, NativePreparation, NativePreparationCancellation, NativeProcessing,
    ProcessingReady,
};
pub use group::{MAX_MUTATION_GROUP, MutationRequest};
pub use lifecycle::Maintenance;
pub use managed_activation::{
    ManagedActivationCause, ManagedActivationRequest, ManagedActivationResult,
    VerifiedManagedActivation,
};

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    Error, ErrorCode, Result,
    model::Context,
    ports::{HostGate, Storage},
    project::ProjectChannelPolicy,
    sequence::Sequence,
    time::Clock,
};
use work::Work;

/// One committed result. Signals may be emitted only after this is returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Committed {
    /// Stable successful public result, including on a semantic retry.
    pub result: Value,
    /// Last newly committed event; a replay/no-event operation returns none.
    pub event_head: Option<Sequence>,
}

/// Host-independent application service shared by every transport.
///
/// The runtime supplies a bounded, authoritative host gate and serializes work.
/// CPU-intensive glob compilation/intersection uses Tokio blocking workers.
/// Storage still holds its immediate transaction while deciding exclusivity.
pub struct Application {
    storage: Arc<dyn Storage>,
    clock: Arc<dyn Clock>,
    host: Arc<dyn HostGate>,
    launch_cursor: std::sync::Mutex<Option<Uuid>>,
}

impl Application {
    /// Compose the actual project store, time source and live host verifier.
    pub fn new(storage: Arc<dyn Storage>, clock: Arc<dyn Clock>, host: Arc<dyn HostGate>) -> Self {
        Self {
            storage,
            clock,
            host,
            launch_cursor: std::sync::Mutex::new(None),
        }
    }

    /// Stable project namespace, taken from storage rather than caller routing.
    pub fn project_id(&self) -> Uuid {
        self.storage.project_id()
    }

    /// Create the default channel once as part of resumable project initialization.
    pub async fn initialize(&self) -> Result<Committed> {
        initialize_project(self.storage.as_ref(), self.clock.as_ref()).await
    }

    /// Apply optional project default-channel setup through shared policy.
    /// Omission preserves stored policy and materializes the legacy `[dev]`
    /// interpretation only when no policy has ever been configured.
    pub async fn initialize_with_defaults(
        &self,
        defaults: Option<Vec<String>>,
    ) -> Result<Committed> {
        let supplied = defaults.map(ProjectChannelPolicy::new).transpose()?;
        configure_project(self.storage.as_ref(), self.clock.as_ref(), supplied).await
    }

    /// Configure defaults and receipt settings in one shared transaction.
    /// Null/omitted receipt fields preserve the current namespace policy.
    pub async fn initialize_with_settings(
        &self,
        defaults: Option<Vec<String>>,
        settings: crate::project::ReceiptSettingsUpdate,
    ) -> Result<Committed> {
        let supplied = defaults.map(ProjectChannelPolicy::new).transpose()?;
        configure_project_with_settings(
            self.storage.as_ref(),
            self.clock.as_ref(),
            supplied,
            settings,
        )
        .await
    }

    /// Execute one business mutation and retain its exact successful result.
    ///
    /// A replay is checked before mutable participation/lease state. Join
    /// receipts still require their current original managed owner; captured
    /// native metadata errors cannot turn a replay into another activation.
    /// Errors drop the transaction: state, event skeletons, recipient rows and
    /// idempotency keys cannot escape a failed operation.
    pub async fn execute(
        &self,
        request_id: Uuid,
        context: &Context,
        command: Mutation,
    ) -> Result<Committed> {
        let mut prepared = self.prepare_request(MutationRequest {
            request_id,
            context: context.clone(),
            command,
        })?;
        prepared.capture_state(self.host.as_ref()).await;
        let mut tx = self.storage.begin().await?;
        let result = self.apply_prepared(tx.as_mut(), prepared).await?;
        tx.commit().await?;
        Ok(result)
    }

    fn validate_context(&self, context: &Context) -> Result<()> {
        if context.project_id != self.project_id() {
            return Err(Error::new(
                ErrorCode::ProjectBindingMismatch,
                "Context belongs to a different project.",
            ));
        }
        if context.host_session_id.is_empty()
            || context.host_session_id.len() > 1024
            || context.host_version.is_empty()
            || context.host_version.len() > 128
            || context.os_profile.is_empty()
            || context.os_profile.len() > 128
            || !context.working_directory.is_absolute()
        {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Exact bounded host descriptor is required.",
            ));
        }
        Ok(())
    }
}

/// Initialize the default work area without manufacturing a host conversation.
/// Project-reference installation and recovery invoke this same application
/// operation before exposing a new namespace to ordinary clients.
pub async fn initialize_project(storage: &dyn Storage, clock: &dyn Clock) -> Result<Committed> {
    configure_project(storage, clock, None).await
}

/// Configure the project defaults and their mandatory current memberships in
/// one transaction. A missing stored row is the legacy `[dev]` policy; writing
/// it here makes later setup intent explicit without changing effective policy.
pub async fn configure_project(
    storage: &dyn Storage,
    clock: &dyn Clock,
    supplied: Option<ProjectChannelPolicy>,
) -> Result<Committed> {
    configure_project_with_settings(storage, clock, supplied, Default::default()).await
}

/// Atomically apply channel and receipt configuration through the common writer.
/// The installer retains request identity/results around this same operation.
pub async fn configure_project_with_settings(
    storage: &dyn Storage,
    clock: &dyn Clock,
    supplied: Option<ProjectChannelPolicy>,
    settings: crate::project::ReceiptSettingsUpdate,
) -> Result<Committed> {
    settings.validate()?;
    let mut tx = storage.begin().await?;
    let mut work = Work {
        tx: tx.as_mut(),
        now: clock.now(),
        last_seq: None,
    };
    let previous_settings = work.tx.receipt_settings().await?;
    let receipt_settings = settings.apply(previous_settings)?;
    let receipt_settings_changed = receipt_settings != previous_settings;
    if receipt_settings_changed {
        work.tx.save_receipt_settings(&receipt_settings).await?;
    }
    let stored = work.tx.channel_policy().await?;
    let previous = stored.clone().unwrap_or_else(ProjectChannelPolicy::legacy);
    let policy = supplied.unwrap_or_else(|| previous.clone());
    let changed = policy != previous;
    let materialized = stored.is_none();
    let removed_memberships = if materialized {
        participation::cutover_legacy_memberships(&mut work, &policy).await?
    } else {
        0
    };
    participation::ensure_default_memberships(&mut work, &policy).await?;
    if stored.as_ref() != Some(&policy) {
        work.tx.save_channel_policy(&policy).await?;
    }
    if materialized {
        work.informational(
            "project.channel_policy_materialized",
            None,
            None,
            serde_json::json!({
                "default_channels": policy.ordered_defaults(),
                "primary_channel": policy.primary(),
                "removed_legacy_memberships": removed_memberships,
            }),
        )
        .await?;
    } else if changed {
        work.informational(
            "project.channel_policy_changed",
            None,
            None,
            serde_json::json!({
                "default_channels": policy.ordered_defaults(),
                "primary_channel": policy.primary(),
            }),
        )
        .await?;
    }
    let result = serde_json::json!({
        "default_channels": policy.ordered_defaults(),
        "primary_channel": policy.primary(),
        "changed": changed,
        "materialized": materialized,
        "removed_legacy_memberships": removed_memberships,
        "receipt_settings": receipt_settings,
        "receipt_settings_changed": receipt_settings_changed,
    });
    let event_head = work.last_seq;
    tx.commit().await?;
    Ok(Committed { result, event_head })
}
