//! Removal reuses canonical discovery but permits an already-absent namespace.

use super::{Project, ProjectManager, discovery, fileio, git, journal, metadata, retirement};
use agentchat_core::project::ProjectIdentity;
use agentchat_core::{Error, ErrorCode, Result};
use std::path::Path;
use uuid::Uuid;

impl ProjectManager {
    pub(crate) fn removal_handle(
        &self,
        identity: ProjectIdentity,
        root: &Path,
        cwd: &Path,
    ) -> Result<Project> {
        metadata::Reference::new(&identity).validate()?;
        let layout = git::inspect(cwd)?;
        let valid = if identity.scope == agentchat_core::project::Scope::Repository {
            layout
                .repository
                .as_ref()
                .is_some_and(|repo| repo.common == root)
        } else {
            cwd.starts_with(root)
                && layout
                    .boundary
                    .as_ref()
                    .is_none_or(|boundary| root.starts_with(boundary))
        };
        if !valid || root.canonicalize().map_err(fileio::io)? != root {
            return Err(metadata::mismatch());
        }
        metadata::Binding::new(identity, root).project(self)
    }
    pub(crate) fn removal_project(&self, path: &Path) -> Result<Option<Project>> {
        let cwd = discovery::canonical(path)?;
        let layout = git::inspect(&cwd)?;
        let Some(located) = discovery::locate(&cwd, &layout)? else {
            return Ok(None);
        };
        let identity = located.reference.identity();
        let data = self.home.join(identity.id.to_string());
        match std::fs::symlink_metadata(&data) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A valid stale reference authorizes only its own removal.
                return Ok(Some(
                    metadata::Binding::new(identity, &located.root).project(self)?,
                ));
            }
            Err(e) => return Err(fileio::io(e)),
            Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                return Err(fileio::conflict(
                    "Present project namespace has a foreign type; it was preserved.",
                ));
            }
            Ok(_) => (),
        }
        let binding = metadata::read_binding(self, &identity).map_err(|_| {
            let mut error = Error::new(ErrorCode::ProjectBindingMismatch,
                "Present project data has missing or untrusted UUID/binding authority; uninstall preserved it.");
            error.details = serde_json::json!({"reason":"untrusted_present_namespace","project_id":identity.id});
            error
        })?;
        if binding.root()? != located.root {
            return Err(metadata::mismatch());
        }
        Ok(Some(binding.project(self)?))
    }

    pub(crate) fn retire_removal_requests(&self, project: Option<Uuid>) -> Result<()> {
        retirement::retire(self, project)?;
        if let Some(pending) = journal::pending(self)?
            && project.is_none_or(|id| id == pending.receipt.binding.identity.id)
        {
            journal::clear(self)?;
        }
        Ok(())
    }
}
