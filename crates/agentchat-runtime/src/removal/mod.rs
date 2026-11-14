//! Shared administrative removal policy and durable same-request recovery.
mod activity;
mod global;
mod global_plan;
mod io;
mod journal;
mod native;
mod plan;
mod tree;

use crate::project::ProjectManager;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::removal::{RemovalOutcome, RemovalScope};
use plan::{Plan, Request};
use std::{path::Path, sync::Arc};
use uuid::Uuid;

/// Durable removal milestones for bounded instrumentation and crash fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalStage {
    /// Complete validated authority/inode plan is durable outside all targets.
    Prepared,
    /// Associated setup requests are durably retired before any removal.
    ReceiptsRetired,
    /// Configuration preimages are private and durable outside deletion targets.
    ConfigurationBackedUp,
    /// One preserving host edit or exclusive directory removal is durable.
    IntegrationRemoved {
        /// Zero-based host component in the frozen plan.
        index: usize,
    },
    /// One exact original inode was unlinked and its parent synchronized.
    NodeRemoved {
        /// Zero-based component in the frozen deletion plan.
        component: usize,
        /// Original inventory position, retained across partial retries.
        node: usize,
    },
    /// A complete component removal is durably recorded.
    ComponentRemoved {
        /// Zero-based component in the frozen deletion plan.
        index: usize,
    },
    /// Immutable completion result is durable; pending intent may remain.
    ReceiptPublished,
    /// Pending intent cleared durably; repeating the key only returns its result.
    Complete,
}
/// Short nonblocking observation; failure preserves the durable removal intent.
pub trait RemovalProgress: Send + Sync {
    /// Observe a durable boundary without changing authorization/deletion policy.
    fn reached(&self, stage: RemovalStage) -> Result<()>;
}
pub(crate) struct Silent;
impl RemovalProgress for Silent {
    fn reached(&self, _: RemovalStage) -> Result<()> {
        Ok(())
    }
}

impl ProjectManager {
    /// Observe removal boundaries; tests use disposable roots and never redirect
    /// native host homes or weaken production authority/ownership guards.
    pub fn with_removal_progress(mut self, progress: Arc<dyn RemovalProgress>) -> Self {
        self.removal_progress = progress;
        self
    }

    /// Remove only the effective canonical project's reference/authorized UUID.
    /// A missing reference is already absent and never triggers a UUID scan.
    /// Host integrations and all other namespaces remain outside this scope.
    pub async fn uninstall_project(
        &self,
        path: impl AsRef<Path>,
        request_id: Uuid,
    ) -> Result<RemovalOutcome> {
        let environment = agentchat_hosts::setup::Environment::capture();
        self.uninstall_project_with_environment(path, request_id, environment)
            .await
    }
    /// Use the composition root's captured host destinations for dangerous-root
    /// checks. This changes neither native homes nor installation ownership.
    pub async fn uninstall_project_with_environment(
        &self,
        path: impl AsRef<Path>,
        request_id: Uuid,
        environment: agentchat_hosts::setup::Environment,
    ) -> Result<RemovalOutcome> {
        let manager = self.clone();
        let path = path.as_ref().to_owned();
        crate::project::blocking(move || local(&manager, &path, request_id, &environment)).await
    }
}

fn local(
    manager: &ProjectManager,
    path: &Path,
    id: Uuid,
    environment: &agentchat_hosts::setup::Environment,
) -> Result<RemovalOutcome> {
    if id.is_nil() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Uninstall requires a non-nil request ID.",
        ));
    }
    activity::protected_roots(manager, environment)?;
    let cwd = path.canonicalize().map_err(io)?;
    if !cwd.is_dir() {
        return Err(conflict(
            "Project uninstall requires an existing directory.",
        ));
    }
    let request = Request {
        id,
        scope: RemovalScope::Project,
        cwd,
        state_root: manager.home().into(),
        destinations: Vec::new(),
    };
    let lifecycle = manager.lifecycle()?;
    let fence = lifecycle.exclusive()?;
    let directory = lifecycle.directory();
    let pending = journal::Journal::load(directory)?;
    if let Some(outcome) = journal::completed(directory, &request)? {
        if let Some(pending) = pending
            && pending.plan.request == request
        {
            pending.plan.validate(manager)?;
            if pending.plan.completed != pending.plan.trees.len() {
                return Err(conflict(
                    "Completed uninstall differs from its pending plan.",
                ));
            }
            pending.clear()?;
        }
        return Ok(outcome);
    }
    let mut legacy = Vec::new();
    for path in [
        manager.home().join("init.lock"),
        manager.home().join("integrations/install.lock"),
    ] {
        if let Some(file) = activity::existing_lock(&path)? {
            legacy.push(file);
        }
    }
    let recovered = pending.is_some();
    let mut work = if let Some(pending) = pending {
        if pending.plan.request != request {
            if pending.plan.request.id == request.id {
                return Err(journal::identity_conflict());
            }
            let mut error = conflict(
                "Another unfinished uninstall must be recovered with its original scope and request ID.",
            );
            error.details = serde_json::json!({"reason":"removal_recovery_required","request_id":pending.plan.request.id,"scope":pending.plan.request.scope,"recovery_directory":agentchat_protocol::removal::RemovalPath::new(directory)});
            return Err(error);
        }
        pending.plan.validate(manager)?;
        pending
    } else {
        let selected = manager.removal_project(&request.cwd)?;
        if let Some(project) = &selected
            && let Some(file) = activity::existing_lock(
                &project
                    .reference_path()
                    .parent()
                    .expect("reference directory")
                    .join("init.lock"),
            )?
        {
            legacy.push(file);
        }
        let _active = if let Some(project) = &selected {
            lifecycle.quiescent(&fence, Some(project.identity().id))?
        } else {
            Vec::new()
        };
        if selected.is_some() {
            agentchat_hosts::launch::Registry::new(manager.home())
                .require_quiescent(selected.as_ref().map(|p| p.identity().id))?;
        }
        if let Some(project) = &selected {
            if let Some(file) = activity::existing_lock(&project.data_dir().join("runtime.lock"))? {
                legacy.push(file);
            }
            activity::runtime_directory(project)?;
        }
        let plan = Plan::local(manager, request.clone())?;
        for tree in &plan.trees {
            activity::protected_target(&tree.path, environment)?;
        }
        fence.validate()?;
        let pending = journal::Journal::begin(directory, plan)?;
        if let Err(error) = manager.removal_progress.reached(RemovalStage::Prepared) {
            return Err(partial(error, &pending.plan, directory));
        }
        pending
    };
    let result = (|| {
        for tree in &work.plan.trees {
            activity::protected_target(&tree.path, environment)?;
        }
        let selected = work.plan.handle(manager)?;
        let _active = if let Some(project) = &selected {
            lifecycle.quiescent(&fence, Some(project.identity().id))?
        } else {
            Vec::new()
        };
        if selected.is_some() {
            agentchat_hosts::launch::Registry::new(manager.home())
                .require_quiescent(selected.as_ref().map(|p| p.identity().id))?;
        }
        if recovered && let Some(project) = &selected {
            for path in [
                project.data_dir().join("runtime.lock"),
                project
                    .reference_path()
                    .parent()
                    .expect("reference directory")
                    .join("init.lock"),
            ] {
                if let Some(file) = activity::existing_lock(&path)? {
                    legacy.push(file);
                }
            }
            activity::runtime_directory(project)?;
        }
        if let Some(project) = &selected {
            let current =
                crate::context::ContextRegistry::removal_records(manager, project.identity().id)?;
            if current
                .iter()
                .any(|id| work.plan.contexts.binary_search(id).is_err())
            {
                return Err(conflict(
                    "A new selected-project descriptor arrived after planning; it was preserved.",
                ));
            }
        }
        for tree in &work.plan.trees {
            tree.validate_remaining()?;
        }
        if !work.plan.retired {
            if let Some(selected) = &work.plan.project {
                manager.retire_removal_requests(Some(selected.identity.id))?;
            }
            work.plan.retired = true;
            work.save()?;
            manager
                .removal_progress
                .reached(RemovalStage::ReceiptsRetired)?;
        }
        while work.plan.completed < work.plan.trees.len() {
            fence.validate()?;
            let index = work.plan.completed;
            work.plan.trees[index].apply(|node| {
                manager.removal_progress.reached(RemovalStage::NodeRemoved {
                    component: index,
                    node,
                })
            })?;
            work.plan.completed += 1;
            work.save()?;
            manager
                .removal_progress
                .reached(RemovalStage::ComponentRemoved { index })?;
        }
        for tree in &work.plan.trees {
            tree.require_absent()?;
        }
        let outcome = work.plan.outcome(directory, recovered);
        journal::finish(directory, &request, &outcome)?;
        manager
            .removal_progress
            .reached(RemovalStage::ReceiptPublished)?;
        work.clear()?;
        manager.removal_progress.reached(RemovalStage::Complete)?;
        Ok(outcome)
    })();
    result.map_err(|error| partial(error, &work.plan, directory))
}

fn partial(mut error: Error, plan: &Plan, directory: &Path) -> Error {
    let cause = std::mem::take(&mut error.details);
    error.details = serde_json::json!({"scope":plan.request.scope,"request_id":plan.request.id,
        "data_loss":plan.outcome(directory,true).data_loss,"completed":plan.component_status(true),
        "remaining":plan.component_status(false),
        "remaining_tree_count":plan.trees.len()-plan.completed,
        "present_targets":plan.trees.iter().filter(|t|t.path.exists()).take(32).map(|t|t.component.as_str()).collect::<Vec<_>>(),
        "present_target_count":plan.trees.iter().filter(|t|t.path.exists()).count(),
        "recovery_directory":agentchat_protocol::removal::RemovalPath::new(directory),"backup_directory":plan.backup_directory(directory),
        "reason":"partial_removal","cause":cause});
    error
}
fn conflict(message: &str) -> Error {
    Error::new(ErrorCode::ConfigurationConflict, message)
}
fn io(_: std::io::Error) -> Error {
    Error::new(
        ErrorCode::IoError,
        "Administrative removal filesystem operation failed.",
    )
}
