//! Whole-root authority never derives project bindings from path resemblance.
use super::{
    activity, conflict, io,
    plan::{Plan, Request},
    tree::Tree,
};
use crate::{daemon::RuntimePaths, project::ProjectManager};
use agentchat_core::Result;
use agentchat_hosts::{
    install::{RemovalPlan, Remover},
    lifecycle::{Fence, Lifecycle},
};
use agentchat_protocol::removal::{RemovalOutcome, RemovalPath, RemovalState};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

impl Plan {
    pub fn global(
        manager: &ProjectManager,
        request: Request,
        host: RemovalPlan,
        lifecycle: &Lifecycle,
        fence: &Fence,
    ) -> Result<Self> {
        let mut namespaces = lifecycle
            .recorded_projects(fence)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        if manager.home().is_dir() {
            for (index, entry) in fs::read_dir(manager.home()).map_err(io)?.enumerate() {
                if index >= 65_536 {
                    return Err(conflict("State-root inventory exceeds its bound."));
                }
                let entry = entry.map_err(io)?;
                if let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .filter(|id| !id.is_nil())
                {
                    // Exact UUID entries locate legacy lock/socket paths only.
                    // Whole-root deletion is explicit; no binding is inferred.
                    namespaces.insert(id);
                }
            }
        }
        if namespaces.len() > 4096 {
            return Err(conflict("Global endpoint inventory exceeds its bound."));
        }
        let mut trees = vec![Tree::capture("all_project_state", manager.home().into())?];
        trees[0].reject_reference_overlap()?;
        for id in &namespaces {
            trees.push(Tree::capture(
                &format!("project_runtime:{id}"),
                activity::runtime_for_data(&manager.home().join(id.to_string()))?,
            )?);
        }
        let plan = Self {
            version: 1,
            request,
            project: None,
            trees,
            retired: false,
            completed: 0,
            host: Some(host),
            host_completed: 0,
            backed_up: false,
            namespaces: namespaces.into_iter().collect(),
            contexts: Vec::new(),
        };
        plan.validate(manager)?;
        Ok(plan)
    }
    pub fn validate_global(&self, manager: &ProjectManager) -> Result<()> {
        let host = self
            .host
            .as_ref()
            .ok_or_else(|| conflict("Global journal lacks host cleanup plan."))?;
        if self.project.is_some()
            || !self.contexts.is_empty()
            || !self.request.cwd.as_os_str().is_empty()
            || self.namespaces.len() > 4096
            || host.request_id() != self.request.id
            || self.host_completed > host.len()
            || self.trees.len() != self.namespaces.len() + 1
            || self.host_completed > 0 && !self.backed_up
            || self.completed > 0
                && (!self.retired || self.host_completed != host.len() || !self.backed_up)
            || self.namespaces.windows(2).any(|ids| ids[0] >= ids[1])
            || self.namespaces.iter().any(Uuid::is_nil)
            || self.trees.iter().map(Tree::len).sum::<usize>() > 65_536
        {
            return Err(conflict(
                "Invalid global removal scope, progress or inventory.",
            ));
        }
        let mut expected = vec![("all_project_state".to_owned(), manager.home().to_owned())];
        for id in &self.namespaces {
            expected.push((
                format!("project_runtime:{id}"),
                runtime_path(&manager.home().join(id.to_string()))?,
            ));
        }
        for (tree, (component, path)) in self.trees.iter().zip(expected) {
            if tree.component != component || tree.path != path || tree.is_file() {
                return Err(conflict(
                    "Global tree differs from its explicit selected root/endpoint locator.",
                ));
            }
            tree.validate_format()?;
        }
        for (i, tree) in self.trees.iter().enumerate() {
            for other in self.trees.iter().skip(i + 1) {
                if tree.path.starts_with(&other.path) || other.path.starts_with(&tree.path) {
                    return Err(conflict(
                        "Global state and endpoint deletion targets overlap.",
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn validate_destinations(&self, remover: &Remover, fence: &Fence) -> Result<()> {
        let destinations = remover.destinations()?;
        let bytes = destinations
            .iter()
            .map(|p| p.as_os_str().as_encoded_bytes().to_vec())
            .collect::<Vec<_>>();
        if bytes != self.request.destinations {
            return Err(conflict(
                "Captured global integration destinations changed.",
            ));
        }
        for path in destinations {
            for tree in &self.trees {
                if tree.path.starts_with(&path) || path.starts_with(&tree.path) {
                    return Err(conflict(
                        "Global state/runtime overlaps a host integration destination.",
                    ));
                }
            }
        }
        remover.validate(self.host.as_ref().expect("validated host plan"), fence)
    }
    pub fn global_outcome(&self, directory: &Path, recovered: bool) -> RemovalOutcome {
        RemovalOutcome {
            scope:self.request.scope,
            state:if self.trees.iter().any(Tree::present) || self.host.as_ref().is_some_and(|h| !h.is_empty()) { RemovalState::Removed } else { RemovalState::AlreadyAbsent },
            project_id:None,
            data_loss:"All projects' databases, history, bindings, contexts and installation/runtime records under the selected state root are removed. Project-local references remain. The private configuration backup does not promise a history backup.".into(),
            completed:self.component_status(true), backup_directory:self.backup_directory(directory),
            recovery_directory:RemovalPath::new(directory), project_references_retained:true, recovered,
        }
    }
}
fn runtime_path(data: &Path) -> Result<PathBuf> {
    let path = RuntimePaths::for_data_directory(data).directory;
    Ok(path
        .parent()
        .expect("runtime parent")
        .canonicalize()
        .map_err(io)?
        .join(path.file_name().expect("runtime name")))
}
