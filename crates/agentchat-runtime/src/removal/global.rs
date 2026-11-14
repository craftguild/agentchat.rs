//! Host cleanup precedes whole-state deletion under one surviving root fence.
use super::{
    RemovalStage, activity, conflict, journal, partial,
    plan::{Plan, Request},
};
use crate::{daemon::RuntimePaths, project::ProjectManager};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::{
    install::Remover,
    lifecycle::{Fence, Lifecycle},
    setup::Environment,
};
use agentchat_protocol::removal::{RemovalOutcome, RemovalScope};
use std::fs::File;
use uuid::Uuid;

impl ProjectManager {
    /// Remove exact user-wide integrations and the entire selected state root.
    /// This is context-free: repositories and native conversations are not read.
    pub async fn uninstall_global(&self, request_id: Uuid) -> Result<RemovalOutcome> {
        self.uninstall_global_with_environment(request_id, Environment::capture())
            .await
    }
    /// Use the composition root's captured host overrides, including isolated
    /// temporary roots. No native executable is invoked or stopped by removal.
    pub async fn uninstall_global_with_environment(
        &self,
        request_id: Uuid,
        environment: Environment,
    ) -> Result<RemovalOutcome> {
        let manager = self.clone();
        crate::project::blocking(move || global(&manager, request_id, environment)).await
    }
}

fn global(manager: &ProjectManager, id: Uuid, environment: Environment) -> Result<RemovalOutcome> {
    if id.is_nil() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Uninstall requires a non-nil request ID.",
        ));
    }
    activity::protected_roots(manager, &environment)?;
    let remover = Remover::new(manager.home(), environment)?;
    let request = Request {
        id,
        scope: RemovalScope::Global,
        cwd: Default::default(),
        state_root: manager.home().into(),
        destinations: remover
            .destinations()?
            .iter()
            .map(|p| p.as_os_str().as_encoded_bytes().to_vec())
            .collect(),
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
            if pending.plan.completed != pending.plan.trees.len()
                || pending.plan.host_completed
                    != pending
                        .plan
                        .host
                        .as_ref()
                        .expect("validated host plan")
                        .len()
            {
                return Err(conflict(
                    "Completed global removal differs from pending progress.",
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
                "Recover the unfinished uninstall with its original scope and request ID first.",
            );
            error.details = serde_json::json!({"reason":"removal_recovery_required","request_id":pending.plan.request.id,"scope":pending.plan.request.scope,"recovery_directory":agentchat_protocol::removal::RemovalPath::new(directory)});
            return Err(error);
        }
        pending.plan.validate(manager)?;
        pending
    } else {
        let _active = lifecycle.quiescent(&fence, None)?;
        agentchat_hosts::launch::Registry::new(manager.home()).require_quiescent(None)?;
        let host = remover.prepare(id, &fence)?;
        let plan = Plan::global(manager, request.clone(), host, &lifecycle, &fence)?;
        let _legacy = quiescence(manager, &plan, &lifecycle, &fence)?;
        plan.validate_destinations(&remover, &fence)?;
        fence.validate()?;
        let pending = journal::Journal::begin(directory, plan)?;
        if let Err(error) = manager.removal_progress.reached(RemovalStage::Prepared) {
            return Err(partial(error, &pending.plan, directory));
        }
        pending
    };
    let result = (|| {
        let _active = lifecycle.quiescent(&fence, None)?;
        let _legacy = quiescence(manager, &work.plan, &lifecycle, &fence)?;
        work.plan.validate_destinations(&remover, &fence)?;
        for tree in &work.plan.trees {
            tree.validate_remaining()?;
        }
        if !work.plan.retired {
            manager.retire_removal_requests(None)?;
            work.plan.retired = true;
            work.save()?;
            manager
                .removal_progress
                .reached(RemovalStage::ReceiptsRetired)?;
        }
        // Revalidate the private preimages on every recovery, even after host
        // edits: absent restoration metadata cannot silently become success.
        remover.backup(
            work.plan.host.as_ref().expect("validated host plan"),
            &fence,
        )?;
        if !work.plan.backed_up {
            work.plan.backed_up = true;
            work.save()?;
            manager
                .removal_progress
                .reached(RemovalStage::ConfigurationBackedUp)?;
        }
        let host_len = work.plan.host.as_ref().expect("validated host plan").len();
        while work.plan.host_completed < host_len {
            let index = work.plan.host_completed;
            remover.apply(
                work.plan.host.as_ref().expect("validated host plan"),
                index,
                &fence,
            )?;
            work.plan.host_completed += 1;
            work.save()?;
            manager
                .removal_progress
                .reached(RemovalStage::IntegrationRemoved { index })?;
        }
        while work.plan.completed < work.plan.trees.len() {
            fence.validate()?;
            // Recheck host files too: a changed preimage never permits state loss.
            work.plan.validate_destinations(&remover, &fence)?;
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
        work.plan.validate_destinations(&remover, &fence)?;
        remover.backup(
            work.plan.host.as_ref().expect("validated host plan"),
            &fence,
        )?;
        let outcome = work.plan.outcome(directory, recovered);
        journal::finish(directory, &request, &outcome)?;
        manager
            .removal_progress
            .reached(RemovalStage::ReceiptPublished)?;
        work.clear()?;
        manager.removal_progress.reached(RemovalStage::Complete)?;
        Ok(outcome)
    })();
    result.map_err(|e| partial(e, &work.plan, directory))
}

fn quiescence(
    manager: &ProjectManager,
    plan: &Plan,
    lifecycle: &Lifecycle,
    fence: &Fence,
) -> Result<Vec<File>> {
    fence.require_exclusive(manager.home())?;
    agentchat_hosts::launch::Registry::new(manager.home()).require_quiescent(None)?;
    let mut held = Vec::new();
    for id in &plan.namespaces {
        let data = manager.home().join(id.to_string());
        if let Some(file) = activity::existing_lock(&RuntimePaths::for_data_directory(&data).lock)?
        {
            held.push(file);
        }
        activity::runtime_for_data(&data)?;
    }
    // The caller retains global activity leases; inventory only locates legacy
    // endpoints and does not turn stale records into activity or project identity.
    lifecycle.recorded_projects(fence)?;
    Ok(held)
}
