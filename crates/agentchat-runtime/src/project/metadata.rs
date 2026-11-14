//! Versioned bounded references and native-path authorization records.

use super::{Project, ProjectManager, fileio};
use agentchat_core::{
    Error, ErrorCode, Result,
    project::{ProjectIdentity, Scope},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    pub version: u32,
    pub project_id: Uuid,
    pub scope: Scope,
}
impl Reference {
    pub fn new(identity: &ProjectIdentity) -> Self {
        Self {
            version: 1,
            project_id: identity.id,
            scope: identity.scope,
        }
    }
    pub fn validate(&self) -> Result<()> {
        version(self.version)?;
        if self.project_id.get_version_num() != 4 || self.project_id.as_bytes()[8] >> 6 != 2 {
            return Err(fileio::conflict(
                "Project references require a complete UUID v4 identity.",
            ));
        }
        Ok(())
    }
    pub fn identity(&self) -> ProjectIdentity {
        ProjectIdentity {
            id: self.project_id,
            scope: self.scope,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub version: u32,
    pub identity: ProjectIdentity,
    pub canonical_location_bytes: Vec<u8>,
}
impl Binding {
    pub fn new(identity: ProjectIdentity, root: &Path) -> Self {
        Self {
            version: 1,
            identity,
            canonical_location_bytes: native(root),
        }
    }
    pub fn validate(&self) -> Result<()> {
        version(self.version)?;
        Reference::new(&self.identity).validate()?;
        native_path(&self.canonical_location_bytes)?;
        Ok(())
    }
    pub fn root(&self) -> Result<PathBuf> {
        native_path(&self.canonical_location_bytes)
    }
    pub fn project(&self, manager: &ProjectManager) -> Result<Project> {
        self.validate()?;
        Ok(Project {
            root: self.root()?,
            data: manager.home.join(self.identity.id.to_string()),
            binding: self.clone(),
        })
    }
}

pub(super) fn native(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}
pub(super) fn native_path(bytes: &[u8]) -> Result<PathBuf> {
    let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    if bytes.len() > 16_384
        || bytes.contains(&0)
        || !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(fileio::conflict(
            "Project binding has an invalid native absolute path.",
        ));
    }
    Ok(path)
}
pub(super) fn version(value: u32) -> Result<()> {
    if value != 1 {
        return Err(Error::new(
            ErrorCode::UnsupportedProtocol,
            "Project metadata format is unsupported.",
        ));
    }
    Ok(())
}
pub(super) fn reference_path(root: &Path, scope: Scope) -> PathBuf {
    root.join(if scope == Scope::Repository {
        "agentchat"
    } else {
        ".agentchat"
    })
    .join("project.json")
}
pub(super) fn read_reference(root: &Path, scope: Scope) -> Result<Option<Reference>> {
    let parent = root.join(if scope == Scope::Repository {
        "agentchat"
    } else {
        ".agentchat"
    });
    if let Ok(meta) = std::fs::symlink_metadata(&parent)
        && (!meta.is_dir() || meta.file_type().is_symlink())
    {
        return Err(fileio::conflict(
            "Project reference directory has a foreign type or symbolic link.",
        ));
    }
    let value: Option<Reference> = fileio::read(&reference_path(root, scope))?;
    if let Some(value) = &value {
        value.validate()?;
        if value.scope != scope {
            return Err(fileio::conflict(
                "Project reference scope does not match its location.",
            ));
        }
    }
    Ok(value)
}
pub(super) fn binding_path(manager: &ProjectManager, id: Uuid) -> PathBuf {
    manager.home.join(id.to_string()).join("project.json")
}
pub(super) fn read_binding(
    manager: &ProjectManager,
    identity: &ProjectIdentity,
) -> Result<Binding> {
    let path = binding_path(manager, identity.id);
    let parent = path
        .parent()
        .ok_or_else(|| fileio::conflict("Project binding has no namespace directory."))?;
    match std::fs::symlink_metadata(parent) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(fileio::conflict(
                "Project namespace directory has a foreign type or symbolic link.",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut error = Error::new(
                ErrorCode::ProjectBindingMismatch,
                "The retained project reference has no namespace. Use agentchat install --new-project, or agentchat uninstall followed by agentchat install; retained history will not be silently recreated.",
            );
            error.details = serde_json::json!({"reason":"retained_namespace_missing"});
            return Err(error);
        }
        Err(error) => return Err(fileio::io(error)),
    }
    let value: Binding = fileio::read(&path)?.ok_or_else(mismatch)?;
    value.validate()?;
    if &value.identity != identity {
        return Err(mismatch());
    }
    Ok(value)
}
pub(super) fn mismatch() -> Error {
    Error::new(
        ErrorCode::ProjectBindingMismatch,
        "Project reference does not match its authorized canonical location.",
    )
}
