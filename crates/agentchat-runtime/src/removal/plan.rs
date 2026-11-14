//! Exact selected-project authority is frozen before deletion is journaled.
use super::{activity, conflict, io, tree::Tree};
use crate::project::{Project, ProjectManager};
use agentchat_core::{Result, project::ProjectIdentity};
use agentchat_protocol::removal::{RemovalOutcome, RemovalPath, RemovalScope, RemovalState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub id: Uuid,
    pub scope: RemovalScope,
    #[serde(with = "super::native")]
    pub cwd: PathBuf,
    #[serde(with = "super::native")]
    pub state_root: PathBuf,
    pub destinations: Vec<Vec<u8>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selected {
    pub identity: ProjectIdentity,
    #[serde(with = "super::native")]
    pub root: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub version: u32,
    pub request: Request,
    pub project: Option<Selected>,
    pub trees: Vec<Tree>,
    pub retired: bool,
    pub completed: usize,
    pub host: Option<agentchat_hosts::install::RemovalPlan>,
    pub host_completed: usize,
    pub backed_up: bool,
    pub namespaces: Vec<Uuid>,
    pub contexts: Vec<Uuid>,
}
impl Plan {
    pub fn local(manager: &ProjectManager, request: Request) -> Result<Self> {
        let project = manager.removal_project(&request.cwd)?;
        let mut trees = Vec::new();
        let mut contexts = Vec::new();
        if let Some(project) = &project {
            trees.push(Tree::capture_reference(
                project
                    .reference_path()
                    .parent()
                    .expect("reference directory")
                    .into(),
            )?);
            trees.push(Tree::capture("project_state", project.data_dir().into())?);
            trees.push(Tree::capture(
                "project_runtime",
                activity::runtime_directory(project)?,
            )?);
            contexts =
                crate::context::ContextRegistry::removal_records(manager, project.identity().id)?;
            for id in &contexts {
                trees.push(Tree::capture_file(
                    &format!("project_context:{id}"),
                    manager.home().join("contexts").join(format!("{id}.json")),
                )?);
            }
        }
        let value = Self {
            version: 1,
            request,
            project: project.map(|p| Selected {
                identity: p.identity().clone(),
                root: p.root().into(),
            }),
            trees,
            retired: false,
            completed: 0,
            host: None,
            host_completed: 0,
            backed_up: false,
            namespaces: Vec::new(),
            contexts,
        };
        value.validate(manager)?;
        Ok(value)
    }
    pub fn handle(&self, manager: &ProjectManager) -> Result<Option<Project>> {
        self.project
            .as_ref()
            .map(|p| manager.removal_handle(p.identity.clone(), &p.root, &self.request.cwd))
            .transpose()
    }
    pub fn validate(&self, manager: &ProjectManager) -> Result<()> {
        if self.version != 1
            || self.request.id.is_nil()
            || self.request.state_root != manager.home()
            || self.completed > self.trees.len()
        {
            return Err(conflict(
                "Removal journal has inconsistent scope, request or state-root authority.",
            ));
        }
        if self.request.scope == RemovalScope::Global {
            return self.validate_global(manager);
        }
        if !self.request.destinations.is_empty()
            || self.host.is_some()
            || self.host_completed != 0
            || self.backed_up
            || !self.namespaces.is_empty()
        {
            return Err(conflict(
                "Project removal journal contains global components.",
            ));
        }
        if self.request.cwd.canonicalize().map_err(io)? != self.request.cwd {
            return Err(conflict("Removal request directory was redirected."));
        }
        if self.contexts.len() > 65_536
            || self.project.is_none() && !self.contexts.is_empty()
            || self.trees.iter().map(Tree::len).sum::<usize>() > 65_536
            || self.contexts.windows(2).any(|p| p[0] >= p[1])
            || self.contexts.iter().any(Uuid::is_nil)
        {
            return Err(conflict("Invalid selected context inventory."));
        }
        let expected = if let Some(project) = self.handle(manager)? {
            let mut paths = vec![
                (
                    "project_reference".to_owned(),
                    project
                        .reference_path()
                        .parent()
                        .expect("reference directory")
                        .to_owned(),
                ),
                ("project_state".to_owned(), project.data_dir().to_owned()),
                ("project_runtime".to_owned(), runtime_path(&project)?),
            ];
            paths.extend(self.contexts.iter().map(|id| {
                (
                    format!("project_context:{id}"),
                    manager.home().join("contexts").join(format!("{id}.json")),
                )
            }));
            paths
        } else {
            Vec::new()
        };
        if expected.len() != self.trees.len() {
            return Err(conflict("Removal journal has an invalid component set."));
        }
        for (index, (tree, (component, path))) in self.trees.iter().zip(expected).enumerate() {
            if tree.component != component || tree.path != path || tree.is_file() != (index >= 3) {
                return Err(conflict(
                    "Removal target differs from its authorized UUID/scope.",
                ));
            }
            tree.validate_format()?;
        }
        for (index, tree) in self.trees.iter().enumerate() {
            for other in self.trees.iter().skip(index + 1) {
                if tree.path.starts_with(&other.path) || other.path.starts_with(&tree.path) {
                    return Err(conflict("Removal targets overlap; all data was preserved."));
                }
            }
        }
        Ok(())
    }
    pub fn outcome(&self, recovery: &Path, recovered: bool) -> RemovalOutcome {
        if self.request.scope == RemovalScope::Global {
            return self.global_outcome(recovery, recovered);
        }
        RemovalOutcome {scope:self.request.scope,state:if self.trees.iter().any(Tree::present){RemovalState::Removed}else{RemovalState::AlreadyAbsent},project_id:self.project.as_ref().map(|p|p.identity.id),data_loss:"Only the selected project's database, history, binding, contexts and runtime artifacts are removed; other projects and global integrations are retained. Configuration recovery records do not promise a history backup.".into(),completed:self.component_status(true),backup_directory:None,recovery_directory:RemovalPath::new(recovery),project_references_retained:false,recovered}
    }
    pub fn backup_directory(&self, recovery: &Path) -> Option<RemovalPath> {
        (self.backed_up && self.host.as_ref().is_some_and(|p| !p.is_empty())).then(|| {
            RemovalPath::new(
                &recovery
                    .join("configuration-backups")
                    .join(self.request.id.to_string()),
            )
        })
    }
    pub fn component_status(&self, completed: bool) -> Vec<String> {
        let mut values = self
            .host
            .as_ref()
            .map(|h| {
                let all = h.components();
                if completed {
                    all[..self.host_completed].to_vec()
                } else {
                    all[self.host_completed..].to_vec()
                }
            })
            .unwrap_or_default();
        let trees = if completed {
            &self.trees[..self.completed]
        } else {
            &self.trees[self.completed..]
        };
        values.extend(trees.iter().take(32).map(|t| t.component.clone()));
        if trees.len() > 32 {
            values.push(format!(
                "additional_runtime_components:{}",
                trees.len() - 32
            ));
        }
        values
    }
}
fn runtime_path(project: &Project) -> Result<PathBuf> {
    let paths = crate::daemon::RuntimePaths::for_project(project);
    let parent = paths
        .directory
        .parent()
        .expect("runtime parent")
        .canonicalize()
        .map_err(io)?;
    Ok(parent.join(paths.directory.file_name().expect("runtime name")))
}
