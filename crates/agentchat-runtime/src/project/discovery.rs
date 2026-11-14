//! Nearest directory overrides with Git repository boundaries and root validation.

use super::{
    Project, ProjectManager, fileio, git,
    metadata::{self, Reference},
};
use agentchat_core::{Result, project::Scope};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) struct Located {
    pub root: PathBuf,
    pub reference: Reference,
}
pub(super) fn canonical(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().map_err(fileio::io)?;
    if !path.is_dir() {
        return Err(fileio::conflict(
            "Project operations require an existing directory.",
        ));
    }
    Ok(path)
}
pub(super) fn locate(cwd: &Path, layout: &git::Layout) -> Result<Option<Located>> {
    for root in cwd.ancestors() {
        if let Some(reference) = metadata::read_reference(root, Scope::Directory)? {
            return Ok(Some(Located {
                root: root.to_owned(),
                reference,
            }));
        }
        if layout.boundary.as_deref() == Some(root) {
            break;
        }
    }
    if let Some(repo) = &layout.repository
        && let Some(reference) = metadata::read_reference(&repo.common, Scope::Repository)?
    {
        return Ok(Some(Located {
            root: repo.common.clone(),
            reference,
        }));
    }
    Ok(None)
}
pub(super) fn discover(manager: &ProjectManager, path: &Path) -> Result<Option<Project>> {
    let cwd = canonical(path)?;
    let layout = git::inspect(&cwd)?;
    let Some(located) = locate(&cwd, &layout)? else {
        return Ok(None);
    };
    let binding = metadata::read_binding(manager, &located.reference.identity())?;
    if binding.root()? != located.root {
        return Err(metadata::mismatch());
    }
    Ok(Some(binding.project(manager)?))
}
pub(super) fn validate(manager: &ProjectManager, project: &Project) -> Result<()> {
    let reference = metadata::read_reference(project.root(), project.identity().scope)?
        .ok_or_else(metadata::mismatch)?;
    if reference.identity() != *project.identity() {
        return Err(metadata::mismatch());
    }
    let binding = metadata::read_binding(manager, project.identity())?;
    if binding != project.binding
        || binding.root()?.canonicalize().map_err(fileio::io)? != project.root
    {
        return Err(metadata::mismatch());
    }
    let db = project.database_path();
    let meta = std::fs::symlink_metadata(db).map_err(fileio::io)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(fileio::conflict(
            "Discovered database is not a regular file.",
        ));
    }
    Ok(())
}
