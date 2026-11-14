//! Private surviving configuration backups and bounded original preimage checks.
use super::super::{conflict, fileio, journal};
use super::{Edit, EmptyDirectory, RemovalPlan, Remover, component, selectors};
use crate::lifecycle::{Fence, Lifecycle};
use agentchat_core::Result;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(super) fn identity(path: &Path) -> Result<((u64, u64), u32)> {
    fileio::safe_parents(path)?;
    let meta = fs::symlink_metadata(path).map_err(super::super::io)?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.nlink() != 1
        || meta.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(conflict(
            "Host removal preimage has foreign file ownership/type.",
        ));
    }
    Ok(((meta.dev(), meta.ino()), meta.mode() & 0o777))
}
pub(super) fn directory_identity(path: &Path) -> Result<((u64, u64), u32)> {
    fileio::safe_parents(&path.join("entry"))?;
    let meta = fs::symlink_metadata(path).map_err(super::super::io)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(conflict(
            "Host removal directory has foreign ownership/type.",
        ));
    }
    Ok(((meta.dev(), meta.ino()), meta.mode() & 0o777))
}
pub(super) fn validate(remover: &Remover, plan: &RemovalPlan, fence: &Fence) -> Result<()> {
    fence.require_exclusive(remover.state())?;
    if plan.id.is_nil()
        || plan.state != fileio::native(remover.state())
        || plan.edits.len() > 16
        || plan.directories.len() > 6
    {
        return Err(conflict("Invalid bounded host removal plan."));
    }
    let ledger = journal::Ledger {
        version: 1,
        roots: journal::roots(&remover.installer),
        entries: plan.ownership.clone(),
    };
    ledger.validate(&remover.installer)?;
    selectors::validate_ledger(remover, &ledger)?;
    let mut paths = BTreeSet::new();
    let mut total = 0usize;
    for edit in &plan.edits {
        let path = fileio::path(&edit.file)?;
        if !paths.insert(path.clone())
            || edit.before.len() > fileio::FILE_LIMIT
            || edit
                .after
                .as_ref()
                .is_some_and(|a| a.len() > fileio::FILE_LIMIT)
            || edit.mode & !0o777 != 0
            || edit.stage.get_version_num() != 4
        {
            return Err(conflict("Invalid or overlapping host removal edit."));
        }
        total += edit.before.len() + edit.after.as_ref().map_or(0, String::len);
        selectors::validate_edit(remover, plan, edit)
            .map_err(|e| component(e, &edit.keys.join("+"), &path))?;
        current(edit).map_err(|e| component(e, &edit.keys.join("+"), &path))?;
    }
    if total > 4 * fileio::FILE_LIMIT {
        return Err(conflict("Host removal preimage set exceeds four MiB."));
    }
    let roots = [
        remover.roots.codex_skill.parent().expect("skill root"),
        remover.roots.claude_skill.parent().expect("skill root"),
    ]
    .into_iter()
    .map(|p| super::super::merge::destination(&remover.installer, p))
    .collect::<Result<Vec<_>>>()?;
    for directory in &plan.directories {
        let path = fileio::path(&directory.path)?;
        let allowed = roots.iter().any(|root| {
            path == *root || path == root.join("hooks") || path == root.join(".claude-plugin")
        });
        if !allowed || !paths.insert(path.clone()) {
            return Err(conflict("Invalid exclusive directory removal target."));
        }
        current_directory(directory)?;
        if path.is_dir() {
            for entry in fs::read_dir(&path).map_err(super::super::io)? {
                let child = entry.map_err(super::super::io)?.path();
                if !plan.edits.iter().any(|e| e.file == fileio::native(&child))
                    && !plan
                        .directories
                        .iter()
                        .any(|d| d.path == fileio::native(&child))
                {
                    return Err(component(
                        conflict(
                            "A foreign artifact member arrived after planning; it was preserved.",
                        ),
                        "integration.directory",
                        &child,
                    ));
                }
            }
        }
    }
    Ok(())
}
pub(super) fn current(edit: &Edit) -> Result<bool> {
    let path = fileio::path(&edit.file)?;
    let actual = fileio::read(&path, fileio::FILE_LIMIT)?;
    if actual == edit.after {
        if actual.is_some()
            && (identity(&path)?.1 != edit.mode
                || directory_identity(path.parent().expect("file parent"))?.0 != edit.parent)
        {
            return Err(conflict(
                "Completed host removal destination permissions/parent changed.",
            ));
        }
        return Ok(true);
    }
    if actual.as_deref() != Some(&edit.before)
        || identity(&path)? != (edit.identity, edit.mode)
        || directory_identity(path.parent().expect("file parent"))?.0 != edit.parent
    {
        return Err(conflict(
            "Host removal preimage changed externally; it was preserved.",
        ));
    }
    Ok(false)
}
fn current_directory(directory: &EmptyDirectory) -> Result<()> {
    let path = fileio::path(&directory.path)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(super::super::io(e)),
        Ok(_) => {
            if directory_identity(&path)? != (directory.identity, directory.mode)
                || directory_identity(path.parent().expect("directory parent"))?.0
                    != directory.parent
            {
                return Err(component(
                    conflict("Exclusive artifact directory identity changed."),
                    "integration.directory",
                    &path,
                ));
            }
            Ok(())
        }
    }
}
pub(super) fn backup(
    remover: &Remover,
    plan: &RemovalPlan,
    fence: &Fence,
) -> Result<Option<PathBuf>> {
    validate(remover, plan, fence)?;
    if plan.is_empty() {
        return Ok(None);
    }
    let directory = Lifecycle::new(remover.state())?
        .directory()
        .join("configuration-backups")
        .join(plan.id.to_string());
    for path in remover.destinations()? {
        if directory.starts_with(&path) || path.starts_with(&directory) {
            return Err(conflict("Configuration backup overlaps a deletion target."));
        }
    }
    fileio::directory(&directory)?;
    for path in [&directory, directory.parent().expect("backup parent")] {
        let meta = fs::symlink_metadata(path).map_err(super::super::io)?;
        if meta.mode() & 0o077 != 0 || meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(conflict(
                "Configuration backup directory is not private and owned.",
            ));
        }
    }
    let text = serde_json::to_string_pretty(plan)
        .map_err(|_| conflict("Cannot encode private configuration backup."))?
        + "\n";
    let path = directory.join("preimages.json");
    if let Some(existing) = fileio::read(&path, fileio::JOURNAL_LIMIT)? {
        if existing != text || fileio::mode(&path)? != 0o600 {
            return Err(conflict("Private configuration backup changed externally."));
        }
        fileio::durable(&path)?;
    } else {
        fileio::atomic(&path, &text, 0o600)?;
    }
    Ok(Some(directory))
}
pub(super) fn apply(
    remover: &Remover,
    plan: &RemovalPlan,
    index: usize,
    fence: &Fence,
) -> Result<()> {
    validate(remover, plan, fence)?;
    if let Some(edit) = plan.edits.get(index) {
        return super::publication::edit(edit);
    }
    let directory = plan
        .directories
        .get(index.saturating_sub(plan.edits.len()))
        .ok_or_else(|| conflict("Unknown host removal component index."))?;
    current_directory(directory)?;
    let path = fileio::path(&directory.path)?;
    if !path.exists() {
        return Ok(());
    }
    super::publication::empty_directory(&path, directory.identity, directory.parent)
}
