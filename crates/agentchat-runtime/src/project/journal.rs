//! Recoverable multi-file publication; successful receipts never rewind roots.

use super::{
    InitRequest, InitStage, ProjectManager, discovery, fileio, git,
    metadata::{self, Binding, Reference},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    project::{ProjectChannelPolicy, ProjectIdentity, ProjectReceiptSettings, Scope},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Normalized {
    pub cwd_bytes: Vec<u8>,
    pub request: InitRequest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub binding: Binding,
    pub created: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    root_bytes: Vec<u8>,
    scope: Scope,
    expected: Option<Reference>,
    desired: Option<Reference>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    version: u32,
    pub request: Normalized,
    pub receipt: Receipt,
    previous: Option<Binding>,
    edits: Vec<Edit>,
    exclude_bytes: Option<Vec<u8>>,
    pub lock_directory_bytes: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Completed {
    pub version: u32,
    pub request: Normalized,
    pub receipt: Receipt,
    #[serde(default)]
    pub default_channels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_settings: Option<ProjectReceiptSettings>,
}
pub(super) fn idempotency_conflict() -> Error {
    Error::new(
        ErrorCode::IdempotencyConflict,
        "Initialization request UUID already has a different actor or normalized arguments.",
    )
}
fn directory(manager: &ProjectManager) -> PathBuf {
    manager.home.join("init")
}
fn pending_path(manager: &ProjectManager) -> PathBuf {
    directory(manager).join("pending.json")
}
fn completed_path(manager: &ProjectManager, id: Uuid) -> PathBuf {
    directory(manager)
        .join("requests")
        .join(format!("{id}.json"))
}
pub(super) fn pending(manager: &ProjectManager) -> Result<Option<Plan>> {
    let plan: Option<Plan> = fileio::read(&pending_path(manager))?;
    if let Some(plan) = &plan {
        validate(plan)?;
    }
    Ok(plan)
}
pub(super) fn completed(manager: &ProjectManager, id: Uuid) -> Result<Option<Completed>> {
    let value: Option<Completed> = fileio::read(&completed_path(manager, id))?;
    if let Some(value) = &value {
        metadata::version(value.version)?;
        value.receipt.binding.validate()?;
        if let Some(settings) = value.receipt_settings {
            settings.validate()?;
        }
        if let Some(defaults) = &value.default_channels
            && ProjectChannelPolicy::new(defaults.clone())?.ordered_defaults() != defaults
        {
            return Err(fileio::conflict(
                "Initialization receipt defaults are not normalized.",
            ));
        }
        metadata::native_path(&value.request.cwd_bytes)?;
        if value.request.request.request_id != id {
            return Err(fileio::conflict(
                "Initialization receipt key does not match its filename.",
            ));
        }
    }
    Ok(value)
}
pub(super) fn clear(manager: &ProjectManager) -> Result<()> {
    fileio::remove(&pending_path(manager))
}
pub(super) fn durable_receipt(manager: &ProjectManager, id: Uuid) -> Result<()> {
    fileio::durable(&completed_path(manager, id))
}
pub(super) fn save_pending(manager: &ProjectManager, plan: &Plan) -> Result<()> {
    fileio::write(&pending_path(manager), plan)
}
pub(super) fn save_completed(manager: &ProjectManager, value: &Completed) -> Result<()> {
    fileio::write(
        &completed_path(manager, value.request.request.request_id),
        value,
    )
}

fn validate(plan: &Plan) -> Result<()> {
    metadata::version(plan.version)?;
    plan.receipt.binding.validate()?;
    metadata::native_path(&plan.request.cwd_bytes)?;
    plan.request.request.validate()?;
    let lock = metadata::native_path(&plan.lock_directory_bytes)?;
    if !matches!(
        lock.file_name().and_then(|n| n.to_str()),
        Some("agentchat" | ".agentchat")
    ) {
        return Err(fileio::conflict(
            "Initialization journal has an invalid lock directory.",
        ));
    }
    if plan.edits.is_empty() || plan.edits.len() > 2 {
        return Err(fileio::conflict(
            "Initialization journal has an invalid edit set.",
        ));
    }
    let target = Reference::new(&plan.receipt.binding.identity);
    if plan.edits[0].desired.as_ref() != Some(&target)
        || plan.edits[0].root_bytes != plan.receipt.binding.canonical_location_bytes
        || plan.edits[0].scope != target.scope
    {
        return Err(fileio::conflict(
            "Initialization journal does not publish its authorized target.",
        ));
    }
    for (i, edit) in plan.edits.iter().enumerate() {
        metadata::native_path(&edit.root_bytes)?;
        for value in edit.expected.iter().chain(edit.desired.iter()) {
            value.validate()?;
            if value.scope != edit.scope {
                return Err(fileio::conflict("Journal edit scope is inconsistent."));
            }
        }
        if i > 0
            && (edit.scope != Scope::Directory
                || edit.desired.is_some()
                || edit.expected.is_none()
                || target.scope != Scope::Repository)
        {
            return Err(fileio::conflict("Journal has an invalid override removal."));
        }
    }
    if let Some(previous) = &plan.previous {
        previous.validate()?;
        if previous.identity != target.identity() {
            return Err(fileio::conflict("Journal cannot rebind another namespace."));
        }
    }
    if let Some(exclude) = &plan.exclude_bytes {
        let path = metadata::native_path(exclude)?;
        if path.file_name() != Some(std::ffi::OsStr::new("exclude"))
            || path.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new("info"))
        {
            return Err(fileio::conflict("Journal Git exclusion target is invalid."));
        }
    }
    Ok(())
}

pub(super) fn prepare(manager: &ProjectManager, request: Normalized) -> Result<Plan> {
    let cwd = metadata::native_path(&request.cwd_bytes)?;
    let layout = git::inspect(&cwd)?;
    let existing = discovery::locate(&cwd, &layout)?;
    let scope = request
        .request
        .scope
        .or_else(|| existing.as_ref().map(|e| e.reference.scope))
        .unwrap_or(if layout.repository.is_some() {
            Scope::Repository
        } else {
            Scope::Directory
        });
    if !request.request.new_project
        && existing
            .as_ref()
            .is_some_and(|e| e.reference.scope != scope)
    {
        return Err(Error::new(
            ErrorCode::ScopeConflict,
            "Explicit scope conflicts with the discovered project reference.",
        ));
    }
    if request.request.relocate && existing.is_none() {
        return Err(Error::new(
            ErrorCode::NotInitialized,
            "No project reference is available for relocation.",
        ));
    }
    let root = if scope == Scope::Repository {
        layout
            .repository
            .as_ref()
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    "Repository scope requires a Git worktree.",
                )
            })?
            .common
            .clone()
    } else if !request.request.new_project {
        existing
            .as_ref()
            .map_or_else(|| cwd.clone(), |e| e.root.clone())
    } else {
        cwd.clone()
    };
    let (binding, previous, created) = if !request.request.new_project
        && let Some(existing) = &existing
    {
        let previous = metadata::read_binding(manager, &existing.reference.identity())?;
        if previous.root()? != root && !request.request.relocate {
            return Err(metadata::mismatch());
        }
        let binding = Binding::new(previous.identity.clone(), &root);
        (binding, Some(previous), false)
    } else {
        let id = (0..32)
            .find_map(|_| {
                let id = Uuid::new_v4();
                match fs::symlink_metadata(manager.home.join(id.to_string())) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Ok(id)),
                    Err(error) => Some(Err(fileio::io(error))),
                    Ok(_) => None,
                }
            })
            .ok_or_else(|| {
                Error::new(ErrorCode::Busy, "Project UUID collision budget exhausted.")
            })??;
        (
            Binding::new(ProjectIdentity { id, scope }, &root),
            None,
            true,
        )
    };
    let expected = metadata::read_reference(&root, scope)?;
    let mut edits = vec![Edit {
        root_bytes: metadata::native(&root),
        scope,
        expected,
        desired: Some(Reference::new(&binding.identity)),
    }];
    if request.request.new_project
        && scope == Scope::Repository
        && let Some(existing) = existing.filter(|e| e.reference.scope == Scope::Directory)
    {
        edits.push(Edit {
            root_bytes: metadata::native(&existing.root),
            scope: Scope::Directory,
            expected: Some(existing.reference),
            desired: None,
        });
    }
    let plan = Plan {
        version: 1,
        request,
        receipt: Receipt { binding, created },
        previous,
        edits,
        exclude_bytes: layout
            .repository
            .as_ref()
            .map(|repo| metadata::native(&repo.exclude)),
        lock_directory_bytes: metadata::native(&if let Some(repo) = &layout.repository {
            repo.common.join("agentchat")
        } else {
            root.join(".agentchat")
        }),
    };
    validate(&plan)?;
    check(manager, &plan)?;
    Ok(plan)
}

pub(super) fn project_lock(plan: &Plan) -> Result<std::fs::File> {
    let path = metadata::native_path(&plan.lock_directory_bytes)?;
    let parent = path
        .parent()
        .ok_or_else(|| fileio::conflict("Project lock has no canonical root."))?;
    if parent.canonicalize().map_err(fileio::io)? != parent {
        return Err(metadata::mismatch());
    }
    fileio::lock(&path)
}

fn check(manager: &ProjectManager, plan: &Plan) -> Result<()> {
    validate(plan)?;
    fileio::database(
        &manager
            .home
            .join(plan.receipt.binding.identity.id.to_string())
            .join("agentchat.sqlite3"),
        !plan.receipt.created,
    )?;
    let root = plan.receipt.binding.root()?;
    if root.canonicalize().map_err(fileio::io)? != root {
        return Err(metadata::mismatch());
    }
    for edit in &plan.edits {
        let actual =
            metadata::read_reference(&metadata::native_path(&edit.root_bytes)?, edit.scope)?;
        if actual != edit.expected && actual != edit.desired {
            return Err(fileio::conflict(
                "Project reference changed during initialization.",
            ));
        }
    }
    let path = metadata::binding_path(manager, plan.receipt.binding.identity.id);
    let actual: Option<Binding> = fileio::read(&path)?;
    if actual.as_ref() != Some(&plan.receipt.binding) && actual != plan.previous {
        return Err(fileio::conflict(
            "Canonical project authorization changed during initialization.",
        ));
    }
    if actual.as_ref() != Some(&plan.receipt.binding)
        && let Some(previous) = &plan.previous
        && previous != &plan.receipt.binding
    {
        match fs::symlink_metadata(previous.root()?) {
            Ok(_) => return Err(metadata::mismatch()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(fileio::io(error)),
        }
    }
    Ok(())
}
pub(super) fn check_and_prepare(manager: &ProjectManager, plan: &Plan) -> Result<()> {
    check(manager, plan)?;
    fileio::private_dir(
        &manager
            .home
            .join(plan.receipt.binding.identity.id.to_string()),
    )
}
pub(super) fn publish(manager: &ProjectManager, plan: &Plan) -> Result<()> {
    check(manager, plan)?;
    let binding_path = metadata::binding_path(manager, plan.receipt.binding.identity.id);
    let current: Option<Binding> = fileio::read(&binding_path)?;
    if current.as_ref() != Some(&plan.receipt.binding) {
        fileio::write(&binding_path, &plan.receipt.binding)?;
    } else {
        fileio::durable(&binding_path)?;
    }
    manager.progress.reached(InitStage::BindingPublished)?;
    for (index, edit) in plan.edits.iter().enumerate() {
        let root = metadata::native_path(&edit.root_bytes)?;
        let current = metadata::read_reference(&root, edit.scope)?;
        if current != edit.expected && current != edit.desired {
            return Err(fileio::conflict(
                "Project reference changed before publication.",
            ));
        }
        let path = metadata::reference_path(&root, edit.scope);
        if let Some(desired) = &edit.desired {
            if current.as_ref() != Some(desired) {
                fileio::write(&path, desired)?;
            } else {
                fileio::durable(&path)?;
            }
        } else if current.is_some() {
            fileio::remove(&path)?;
        } else if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
            fileio::sync_dir(parent)?;
        }
        manager
            .progress
            .reached(InitStage::ReferencePublished { index })?;
    }
    if let Some(path) = &plan.exclude_bytes {
        super::exclude::merge(&metadata::native_path(path)?)?;
    }
    manager.progress.reached(InitStage::ExclusionsUpdated)
}
