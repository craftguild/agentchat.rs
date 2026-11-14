//! Canonical project discovery and recoverable reference initialization.

mod discovery;
mod exclude;
mod fileio;
mod git;
mod init;
mod journal;
mod journal_receipt;
mod metadata;
mod removal;
mod replay;
mod retirement;

pub use init::{InitActor, InitOutcome, InitProgress, InitRequest, InitStage};

use agentchat_core::{Error, ErrorCode, Result, project::ProjectIdentity};
use agentchat_store::Store;
use metadata::Binding;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// A discovered UUID namespace with a captured canonical filesystem binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    binding: Binding,
    root: PathBuf,
    data: PathBuf,
}

impl Project {
    /// Stable namespace and scope, never inferred from the path.
    pub fn identity(&self) -> &ProjectIdentity {
        &self.binding.identity
    }
    /// Authorized canonical directory or Git common directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Private data directory under the configured agentchat home.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }
    /// Small Git-local/directory-local discovery reference.
    pub fn reference_path(&self) -> PathBuf {
        metadata::reference_path(&self.root, self.identity().scope)
    }
    /// Isolated project's SQLite filename.
    pub fn database_path(&self) -> PathBuf {
        self.data.join("agentchat.sqlite3")
    }
}

/// Shared project operations for CLI, MCP and lifecycle discovery.
#[derive(Clone)]
pub struct ProjectManager {
    home: PathBuf,
    progress: Arc<dyn InitProgress>,
    pub(crate) removal_progress: Arc<dyn crate::removal::RemovalProgress>,
}

impl ProjectManager {
    /// Select an explicit agentchat state root without creating it or changing host homes.
    pub fn new(home: impl AsRef<Path>) -> Result<Self> {
        if home.as_ref().as_os_str().is_empty() {
            return Err(Error::new(
                ErrorCode::ConfigurationConflict,
                "AgentChat state root must not be empty.",
            ));
        }
        let home = fileio::home(home.as_ref())?;
        Ok(Self {
            home,
            progress: Arc::new(init::SilentProgress),
            removal_progress: Arc::new(crate::removal::Silent),
        })
    }
    /// Resolve AGENTCHAT_HOME, defaulting to HOME/.agentchat. Environment access is read-only.
    pub fn from_environment() -> Result<Self> {
        let path = match std::env::var_os("AGENTCHAT_HOME") {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            Some(_) => {
                return Err(Error::new(
                    ErrorCode::ConfigurationConflict,
                    "AGENTCHAT_HOME must not be empty.",
                ));
            }
            None => PathBuf::from(
                std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .ok_or_else(|| {
                        Error::new(
                            ErrorCode::ConfigurationConflict,
                            "HOME is unavailable; set AGENTCHAT_HOME explicitly.",
                        )
                    })?,
            )
            .join(".agentchat"),
        };
        Self::new(path)
    }
    /// Install a bounded progress sink. Sink failure leaves resumable intent.
    pub fn with_progress(mut self, progress: Arc<dyn InitProgress>) -> Self {
        self.progress = progress;
        self
    }
    /// Selected agentchat data root; discovery alone never creates this directory.
    pub fn home(&self) -> &Path {
        &self.home
    }
    /// Fence this setup/startup against removal without relying on a lock inode
    /// inside the deletable state tree. Inner project/installer locks still apply.
    pub(crate) async fn installation_fence(&self) -> Result<agentchat_hosts::lifecycle::Fence> {
        let manager = self.clone();
        blocking(move || manager.lifecycle()?.shared()).await
    }
    /// Retain a project lifetime outside the state root, including while its
    /// runtime reconnects. Only this exact UUID contributes to local removal.
    pub(crate) async fn activity(
        &self,
        project: uuid::Uuid,
    ) -> Result<agentchat_hosts::lifecycle::Activity> {
        let manager = self.clone();
        blocking(move || manager.lifecycle()?.activity(project)).await
    }
    pub(crate) fn lifecycle(&self) -> Result<agentchat_hosts::lifecycle::Lifecycle> {
        let selected = agentchat_hosts::lifecycle::Lifecycle::new(&self.home)?;
        if selected.root() != self.home {
            return Err(Error::new(
                ErrorCode::ConfigurationConflict,
                "Captured AgentChat state root was redirected.",
            ));
        }
        Ok(selected)
    }
    /// Cheap, read-only discovery. Uninitialized locations return none.
    pub async fn discover(&self, path: impl AsRef<Path>) -> Result<Option<Project>> {
        let manager = self.clone();
        let path = path.as_ref().to_owned();
        blocking(move || discovery::discover(&manager, &path)).await
    }
    /// Revalidate a discovered handle before opening its ORM storage.
    /// A newer reference cannot silently redirect an older bound conversation.
    pub async fn open(&self, project: &Project) -> Result<Arc<Store>> {
        // Storage can create parent directories during first open. Hold the
        // external lifetime lease before validation so a stale handle cannot
        // cross removal and recreate a deleted namespace.
        let _activity = self.activity(project.identity().id).await?;
        self.validate(project).await?;
        Ok(Arc::new(
            Store::open(project.database_path(), project.identity().clone()).await?,
        ))
    }
    /// Revalidate routing without opening storage or changing participation.
    pub async fn validate(&self, project: &Project) -> Result<()> {
        let manager = self.clone();
        let value = project.clone();
        blocking(move || discovery::validate(&manager, &value)).await
    }
}

pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Error::new(ErrorCode::IoError, "Project filesystem worker failed."))?
}
