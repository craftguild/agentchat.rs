//! Exclusive skill/plugin inventories preserve any unknown or edited member.
use super::super::{Selector, conflict, digest, fileio, journal, merge};
use super::{EmptyDirectory, RemovalPlan, Remover, component, recognize, selectors, verify};
use agentchat_core::Result;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

type DirectoryIdentity = ((u64, u64), u32, (u64, u64));

pub(super) fn prepare(
    remover: &Remover,
    ledger: &journal::Ledger,
    plan: &mut RemovalPlan,
    known: &BTreeMap<String, (PathBuf, Selector)>,
) -> Result<()> {
    let mut files = BTreeMap::new();
    for (key, (path, selector)) in known {
        if *selector == Selector::File
            && let Some(text) = selectors::read(path, key)?
        {
            files.insert(key.clone(), text);
        }
    }
    let identity = files
        .get("claude.skill")
        .is_some_and(|text| recognize::skill(text))
        || files
            .get("claude.native.module")
            .is_some_and(|text| recognize::native_module(text, remover))
        || files
            .get("claude.native.manifest")
            .is_some_and(|text| recognize::manifest(text));
    for (key, text) in &files {
        let (path, selector) = known.get(key).expect("known artifact");
        let recognizable = match key.as_str() {
            "codex.skill" | "claude.skill" => recognize::skill(text),
            "claude.native.module" => recognize::native_module(text, remover),
            "claude.native.input" => identity && recognize::input(text),
            "claude.native.manifest" => recognize::manifest(text),
            "claude.native.modules" => identity && recognize::modules(text),
            _ => false,
        };
        verify(
            ledger,
            key,
            path,
            selector.clone(),
            digest(text.as_bytes()),
            recognizable,
        )?;
        selectors::edit(plan, path, vec![key.clone()], text.clone(), None)?;
    }
    let allowed = known
        .values()
        .filter(|(_, s)| *s == Selector::File)
        .map(|(p, _)| p.clone())
        .collect::<BTreeSet<_>>();
    let mut directories = BTreeMap::new();
    for (key, skill) in [
        ("codex.skill", &remover.roots.codex_skill),
        ("claude.skill", &remover.roots.claude_skill),
    ] {
        let root =
            merge::destination(&remover.installer, skill.parent().expect("skill directory"))?;
        match fs::symlink_metadata(&root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(super::super::io(e)),
            Ok(_) => (),
        }
        let owned = ledger.entries.keys().any(|k| {
            k == key
                || key == "claude.skill"
                    && matches!(
                        k.as_str(),
                        "claude.native.module"
                            | "claude.native.input"
                            | "claude.native.manifest"
                            | "claude.native.modules"
                    )
        });
        let identifiable = if key == "codex.skill" {
            files.get(key).is_some_and(|s| recognize::skill(s))
        } else {
            identity
        };
        if !owned && !identifiable {
            return Err(component(
                conflict(
                    "An exclusive AgentChat destination has no recognizable owned content; it was preserved.",
                ),
                key,
                &root,
            ));
        }
        inventory(&root, &root, &allowed, &mut directories, key)?;
    }
    let mut directories = directories
        .into_iter()
        .map(|(path, (identity, mode, parent))| EmptyDirectory {
            path: fileio::native(&path),
            identity,
            mode,
            parent,
        })
        .collect::<Vec<_>>();
    directories.sort_by_key(|d| std::cmp::Reverse(d.path.len()));
    plan.directories = directories;
    Ok(())
}
fn inventory(
    root: &Path,
    path: &Path,
    allowed: &BTreeSet<PathBuf>,
    directories: &mut BTreeMap<PathBuf, DirectoryIdentity>,
    key: &str,
) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(super::super::io)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(component(
            conflict("AgentChat artifact directory has foreign ownership/type."),
            key,
            path,
        ));
    }
    if path != root && ![root.join("hooks"), root.join(".claude-plugin")].contains(&path.to_owned())
    {
        return Err(component(
            conflict("Unknown directory in AgentChat artifact bundle was preserved."),
            key,
            path,
        ));
    }
    fileio::safe_parents(&path.join("entry"))?;
    let parent = super::files::directory_identity(path.parent().expect("artifact parent"))?.0;
    directories.insert(
        path.into(),
        ((meta.dev(), meta.ino()), meta.mode() & 0o777, parent),
    );
    for (index, entry) in fs::read_dir(path).map_err(super::super::io)?.enumerate() {
        if index >= 16 {
            return Err(component(
                conflict("AgentChat artifact directory exceeds its bounded inventory."),
                key,
                path,
            ));
        }
        let entry = entry.map_err(super::super::io)?;
        let child = entry.path();
        if entry.file_type().map_err(super::super::io)?.is_dir() {
            inventory(root, &child, allowed, directories, key)?;
        } else if !allowed.contains(&child) {
            return Err(component(
                conflict("Unknown AgentChat artifact member was preserved."),
                key,
                &child,
            ));
        } else {
            super::files::identity(&child)?;
        }
    }
    Ok(())
}
