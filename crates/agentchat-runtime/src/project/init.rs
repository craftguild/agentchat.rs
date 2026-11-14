//! Shared project-initialization workflow and durable semantic request identity.

use super::{
    Project, ProjectManager, blocking, discovery, fileio,
    journal::{self, Completed, Plan},
    metadata, retirement,
};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::HostKind,
    project::{ProjectChannelPolicy, ProjectReceiptSettings, ReceiptSettingsUpdate, Scope},
    time::SystemClock,
};
use agentchat_store::Store;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// Initialization actor; a directory never substitutes for a conversation ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InitActor {
    /// Local same-OS-user administration without a host context.
    #[default]
    Local,
    /// Exact actor from verified native initialization or a resolved descriptor.
    Conversation {
        /// Host integration kind.
        host: HostKind,
        /// Exact root host conversation; never a display name or process ID.
        host_session_id: String,
    },
}
/// Initialization flags shared by CLI and MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitRequest {
    /// Transient route freshness condition for bound callers. This is checked
    /// under the initialization lock only for a new request; it is excluded from
    /// stored retry identity so fresh incarnations can replay the same actor/key.
    #[serde(skip)]
    pub expected_project_id: Option<Uuid>,
    /// Stable transport retry key.
    pub request_id: Uuid,
    /// Explicit boundary, or reuse/default selection.
    pub scope: Option<Scope>,
    /// Deliberate replacement namespace; old data is retained.
    pub new_project: bool,
    /// Adopt a moved root only after the old canonical location disappeared.
    pub relocate: bool,
    /// Explicit ordered defaults, or omission to preserve/default project policy.
    pub default_channels: Option<Vec<String>>,
    /// Normalized partial settings update, absent from historical retry records.
    #[serde(default, skip_serializing_if = "no_receipt_settings")]
    pub receipt_settings: ReceiptSettingsUpdate,
    /// Exact request actor, resolved before calling this operation.
    pub actor: InitActor,
}
impl InitRequest {
    /// Ordinary idempotent initialization, without scope/identity replacement.
    pub fn new(request_id: Uuid) -> Self {
        Self {
            expected_project_id: None,
            request_id,
            scope: None,
            new_project: false,
            relocate: false,
            default_channels: None,
            receipt_settings: ReceiptSettingsUpdate::default(),
            actor: InitActor::Local,
        }
    }
    pub(super) fn normalize(&mut self) -> Result<()> {
        self.validate_base()?;
        if let Some(defaults) = self.default_channels.take() {
            self.default_channels = Some(
                ProjectChannelPolicy::new(defaults)?
                    .ordered_defaults()
                    .to_vec(),
            );
        }
        Ok(())
    }
    pub(super) fn validate(&self) -> Result<()> {
        self.validate_base()?;
        if let Some(defaults) = &self.default_channels {
            let normalized = ProjectChannelPolicy::new(defaults.clone())?;
            if normalized.ordered_defaults() != defaults {
                return Err(Error::new(
                    ErrorCode::ConfigurationConflict,
                    "Stored initialization defaults are not normalized.",
                ));
            }
        }
        Ok(())
    }
    fn validate_base(&self) -> Result<()> {
        self.receipt_settings.validate()?;
        if self.new_project && self.relocate {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "New-project and relocate modes are mutually exclusive.",
            ));
        }
        if let InitActor::Conversation {
            host_session_id, ..
        } = &self.actor
            && (host_session_id.is_empty() || host_session_id.len() > 1024)
        {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Initialization actor requires an exact bounded host conversation.",
            ));
        }
        Ok(())
    }
}
/// Project portion of initialization; host integration results are separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// Authorized UUID namespace, including its native filesystem locations.
    pub project: Project,
    /// Whether this successful request created a new namespace.
    /// A replay retains the original value even after subsequent initializations.
    pub created: bool,
    /// Effective ordered defaults returned by this exact successful request.
    pub channel_policy: ProjectChannelPolicy,
    /// Effective settings frozen in this success; old receipts stay unexpanded.
    pub receipt_settings: Option<ProjectReceiptSettings>,
}
/// Durable milestones for bounded progress reporting and recovery testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum InitStage {
    /// Intent is fsynced; no project reference has been published.
    Prepared,
    /// Schema and default channel are committed.
    DatabaseInitialized,
    /// Canonical authorization metadata is durable.
    BindingPublished,
    /// One planned reference edit is durable.
    ReferencePublished {
        /// Zero-based edit position; the new reference precedes override removal.
        index: usize,
    },
    /// Git-local exclusion merge is complete, or no Git merge was needed.
    ExclusionsUpdated,
    /// Successful request result is durable; retry may return it immediately.
    ReceiptPublished,
}
/// A short, nonblocking progress sink; failure preserves resumable intent.
pub trait InitProgress: Send + Sync {
    /// Observe a durable milestone without changing initialization policy.
    fn reached(&self, stage: InitStage) -> Result<()>;
}
pub(super) struct SilentProgress;
impl InitProgress for SilentProgress {
    fn reached(&self, _: InitStage) -> Result<()> {
        Ok(())
    }
}

impl ProjectManager {
    /// Initialize storage/references under agentchat-wide then project OS-held locks.
    /// Repeating a successful key returns its result without republishing an old
    /// reference. Pending intent is recovered before another request proceeds.
    pub async fn initialize(
        &self,
        path: impl AsRef<Path>,
        mut request: InitRequest,
    ) -> Result<InitOutcome> {
        request.normalize()?;
        let expected = request.expected_project_id.take();
        let path = path.as_ref().to_owned();
        let normalized = blocking(move || {
            Ok(journal::Normalized {
                cwd_bytes: metadata::native(&discovery::canonical(&path)?),
                request,
            })
        })
        .await?;
        let _fence = self.installation_fence().await?;
        let manager = self.clone();
        let key = normalized.request.request_id;
        blocking(move || retirement::check(&manager, key)).await?;
        let home = self.home.clone();
        let _lock = blocking(move || fileio::lock(&home)).await?;
        let manager = self.clone();
        let key = normalized.request.request_id;
        blocking(move || retirement::available(&manager, key)).await?;
        let manager = self.clone();
        let pending = blocking(move || journal::pending(&manager)).await?;
        if let Some(plan) = pending {
            let manager = self.clone();
            let key = plan.request.request.request_id;
            blocking(move || retirement::check(&manager, key)).await?;
            if plan.request.request.request_id == normalized.request.request_id
                && plan.request != normalized
            {
                return Err(journal::idempotency_conflict());
            }
            let manager = self.clone();
            let old = plan.clone();
            let completed =
                blocking(move || journal::completed(&manager, old.request.request.request_id))
                    .await?;
            if let Some(completed) = completed {
                if completed.request != plan.request || completed.receipt != plan.receipt {
                    return Err(fileio::conflict(
                        "Pending initialization differs from its completed receipt.",
                    ));
                }
                let manager = self.clone();
                let id = plan.request.request.request_id;
                let namespace = plan.receipt.binding.identity.id;
                blocking(move || {
                    // The result may be visible after a failed rename-directory
                    // sync. Make it durable before discarding recovery intent.
                    retirement::remember(&manager, id, namespace)?;
                    journal::durable_receipt(&manager, id)?;
                    journal::clear(&manager)
                })
                .await?;
            } else {
                let value = plan.clone();
                let _project_lock = blocking(move || journal::project_lock(&value)).await?;
                self.finish(&plan).await?;
            }
        }
        let manager = self.clone();
        let key = normalized.request.request_id;
        if let Some(completed) = blocking(move || journal::completed(&manager, key)).await? {
            if completed.request != normalized {
                return Err(journal::idempotency_conflict());
            }
            let manager = self.clone();
            let namespace = completed.receipt.binding.identity.id;
            blocking(move || retirement::remember(&manager, key, namespace)).await?;
            return completed.outcome(self);
        }
        if let Some(expected) = expected {
            let directory = metadata::native_path(&normalized.cwd_bytes)?;
            if self
                .discover(directory)
                .await?
                .is_none_or(|project| project.identity().id != expected)
            {
                return Err(Error::new(
                    ErrorCode::ProjectBindingMismatch,
                    "Bound initialization reference changed before acquiring its lock.",
                ));
            }
        }
        let manager = self.clone();
        let candidate = normalized.clone();
        let guess = blocking(move || journal::prepare(&manager, candidate)).await?;
        let value = guess.clone();
        let _project_lock = blocking(move || journal::project_lock(&value)).await?;
        let manager = self.clone();
        let plan = blocking(move || journal::prepare(&manager, normalized)).await?;
        if plan.lock_directory_bytes != guess.lock_directory_bytes {
            let mut error = Error::new(
                ErrorCode::Busy,
                "Project boundary changed while acquiring its initialization lock.",
            );
            error.retryable = true;
            return Err(error);
        }
        if !plan.receipt.created {
            // Reject existing corruption/newer schemas before recording intent;
            // an init retry is never permission to rebuild retained storage.
            self.check_existing(&plan).await?;
        }
        let manager = self.clone();
        let intent = plan.clone();
        blocking(move || {
            journal::save_pending(&manager, &intent)?;
            retirement::remember(
                &manager,
                intent.request.request.request_id,
                intent.receipt.binding.identity.id,
            )
        })
        .await?;
        self.progress.reached(InitStage::Prepared)?;
        self.finish(&plan).await
    }

    async fn check_existing(&self, plan: &Plan) -> Result<()> {
        let project = plan.receipt.binding.project(self)?;
        let store = Store::open(project.database_path(), project.identity().clone()).await?;
        store.close().await?;
        Ok(())
    }

    async fn finish(&self, plan: &Plan) -> Result<InitOutcome> {
        let manager = self.clone();
        let value = plan.clone();
        blocking(move || {
            retirement::remember(
                &manager,
                value.request.request.request_id,
                value.receipt.binding.identity.id,
            )?;
            journal::check_and_prepare(&manager, &value)
        })
        .await?;
        let project = plan.receipt.binding.project(self)?;
        let store = Store::open(project.database_path(), project.identity().clone()).await?;
        let supplied = plan
            .request
            .request
            .default_channels
            .clone()
            .map(ProjectChannelPolicy::new)
            .transpose()?;
        let configured = agentchat_core::application::configure_project_with_settings(
            &store,
            &SystemClock,
            supplied,
            plan.request.request.receipt_settings,
        )
        .await?;
        let receipt_settings: ProjectReceiptSettings = serde_json::from_value(
            configured.result["receipt_settings"].clone(),
        )
        .map_err(|_| {
            Error::new(
                ErrorCode::IntegrityError,
                "Project configuration omitted its effective receipt settings.",
            )
        })?;
        let defaults = configured.result["default_channels"]
            .as_array()
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::IntegrityError,
                    "Project configuration omitted its effective defaults.",
                )
            })?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    Error::new(
                        ErrorCode::IntegrityError,
                        "Project configuration returned an invalid default channel.",
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let channel_policy = ProjectChannelPolicy::new(defaults)?;
        store.close().await?;
        self.progress.reached(InitStage::DatabaseInitialized)?;
        let manager = self.clone();
        let value = plan.clone();
        blocking(move || journal::publish(&manager, &value)).await?;
        let manager = self.clone();
        let value = Completed {
            version: 1,
            request: plan.request.clone(),
            receipt: plan.receipt.clone(),
            default_channels: Some(channel_policy.ordered_defaults().to_vec()),
            receipt_settings: Some(receipt_settings),
        };
        blocking(move || journal::save_completed(&manager, &value)).await?;
        self.progress.reached(InitStage::ReceiptPublished)?;
        let manager = self.clone();
        blocking(move || journal::clear(&manager)).await?;
        Ok(InitOutcome {
            project: plan.receipt.binding.project(self)?,
            created: plan.receipt.created,
            channel_policy,
            receipt_settings: Some(receipt_settings),
        })
    }
}

fn no_receipt_settings(settings: &ReceiptSettingsUpdate) -> bool {
    *settings == ReceiptSettingsUpdate::default()
}
