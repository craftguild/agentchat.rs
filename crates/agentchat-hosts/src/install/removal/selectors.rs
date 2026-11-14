//! Supported destinations/selectors are closed; arbitrary ledger paths are not plans.
use super::super::{Selector, conflict, fileio, journal, merge, strict_json, toml, value_digest};
use super::{Edit, RemovalPlan, Remover, component, json_text, recognize, verify};
use agentchat_core::Result;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(super) fn known(remover: &Remover) -> Result<BTreeMap<String, (PathBuf, Selector)>> {
    let r = &remover.roots;
    let plugin = r.claude_skill.parent().expect("skill directory");
    let mut values = vec![
        (
            "codex.mcp",
            r.codex_config.clone(),
            Selector::Toml {
                path: vec!["mcp_servers".into(), "agentchat".into()],
            },
        ),
        (
            "claude.mcp",
            r.claude_registry.clone(),
            Selector::Json {
                path: vec!["mcpServers".into(), "agentchat".into()],
            },
        ),
        ("codex.skill", r.codex_skill.clone(), Selector::File),
        ("claude.skill", r.claude_skill.clone(), Selector::File),
        (
            "claude.native.module",
            plugin.join("hooks/register.js"),
            Selector::File,
        ),
        (
            "claude.native.input",
            plugin.join("hooks/input.js"),
            Selector::File,
        ),
        (
            "claude.native.manifest",
            plugin.join(".claude-plugin/plugin.json"),
            Selector::File,
        ),
        (
            "claude.native.modules",
            plugin.join("hooks/hooks.json"),
            Selector::File,
        ),
    ];
    for (key, event) in [
        ("claude.native.file-changed", "FileChanged"),
        ("claude.native.stop", "Stop"),
        ("claude.native.final", "UserPromptSubmit"),
    ] {
        values.push((
            key,
            r.claude_settings.clone(),
            Selector::JsonArray {
                path: vec!["hooks".into(), event.into()],
            },
        ));
    }
    values
        .into_iter()
        .map(|(key, path, selector)| {
            Ok((
                key.into(),
                (
                    merge::destination(&remover.installer, &path)
                        .map_err(|e| component(e, key, &path))?,
                    selector,
                ),
            ))
        })
        .collect()
}
pub(super) fn destinations(remover: &Remover) -> Result<Vec<PathBuf>> {
    let mut paths = known(remover)?
        .into_values()
        .map(|(p, _)| p)
        .collect::<Vec<_>>();
    for skill in [&remover.roots.codex_skill, &remover.roots.claude_skill] {
        paths.push(merge::destination(
            &remover.installer,
            skill.parent().expect("skill directory"),
        )?);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
pub(super) fn validate_ledger(remover: &Remover, ledger: &journal::Ledger) -> Result<()> {
    let known = known(remover)?;
    for (key, owned) in &ledger.entries {
        let valid = known.get(key).is_some_and(|(path, selector)| {
            owned.file == fileio::native(path) && *selector == owned.selector
        });
        if !valid {
            return Err(component(
                conflict(
                    "Ownership names an unsupported or changed removal destination; it was preserved.",
                ),
                key,
                &fileio::path(&owned.file)?,
            ));
        }
    }
    Ok(())
}
pub(super) fn prepare(
    remover: &Remover,
    ledger: &journal::Ledger,
    plan: &mut RemovalPlan,
) -> Result<()> {
    let known = known(remover)?;
    let (path, selector) = known.get("codex.mcp").expect("known MCP");
    if let Some(before) = read(path, "codex.mcp")? {
        let mut document = toml::parse(&before).map_err(|e| component(e, "codex.mcp", path))?;
        if let Some(parent) = document.get("mcp_servers") {
            let table = parent.as_table_like().ok_or_else(|| {
                component(
                    conflict("Codex MCP parent is not a table."),
                    "codex.mcp",
                    path,
                )
            })?;
            if let Some(item) = table.get("agentchat").filter(|i| !i.is_none()) {
                let value = toml::json(item).map_err(|e| component(e, "codex.mcp", path))?;
                verify(
                    ledger,
                    "codex.mcp",
                    path,
                    selector.clone(),
                    value_digest(&value)?,
                    recognize::mcp(&value, remover, false),
                )?;
                document
                    .get_mut("mcp_servers")
                    .and_then(toml_edit::Item::as_table_like_mut)
                    .expect("validated table")
                    .remove("agentchat");
                edit(
                    plan,
                    path,
                    vec!["codex.mcp".into()],
                    before,
                    Some(document.to_string()),
                )?;
            }
        }
    }
    let (path, selector) = known.get("claude.mcp").expect("known MCP");
    if let Some(before) = read(path, "claude.mcp")? {
        let mut document =
            strict_json::parse(&before).map_err(|e| component(e, "claude.mcp", path))?;
        if let Some(parent) = document.get_mut("mcpServers") {
            let table = parent.as_object_mut().ok_or_else(|| {
                component(
                    conflict("Claude MCP parent is not an object."),
                    "claude.mcp",
                    path,
                )
            })?;
            if let Some(value) = table.get("agentchat") {
                verify(
                    ledger,
                    "claude.mcp",
                    path,
                    selector.clone(),
                    value_digest(value)?,
                    recognize::mcp(value, remover, true),
                )?;
                table.remove("agentchat");
                edit(
                    plan,
                    path,
                    vec!["claude.mcp".into()],
                    before,
                    Some(json_text(&document)?),
                )?;
            }
        }
    }
    super::artifacts::prepare(remover, ledger, plan, &known)?;
    let path = known
        .get("claude.native.stop")
        .expect("known hook")
        .0
        .clone();
    if let Some(before) = read(&path, "claude.hooks")? {
        let mut document =
            strict_json::parse(&before).map_err(|e| component(e, "claude.hooks", &path))?;
        if !document.is_object() {
            return Err(component(
                conflict("Claude settings are not an object."),
                "claude.hooks",
                &path,
            ));
        }
        let keys = super::hooks::clean(&mut document, remover, ledger, &path)?;
        if !keys.is_empty() {
            edit(plan, &path, keys, before, Some(json_text(&document)?))?;
        }
    }
    Ok(())
}
pub(super) fn read(path: &Path, key: &str) -> Result<Option<String>> {
    fileio::read(path, fileio::FILE_LIMIT).map_err(|e| component(e, key, path))
}
pub(super) fn edit(
    plan: &mut RemovalPlan,
    path: &Path,
    keys: Vec<String>,
    before: String,
    after: Option<String>,
) -> Result<()> {
    let (identity, mode) =
        super::files::identity(path).map_err(|e| component(e, &keys.join("+"), path))?;
    if let Some(existing) = plan
        .edits
        .iter_mut()
        .find(|e| e.file == fileio::native(path))
    {
        if existing.before != before
            || existing.after != after
            || existing.mode != mode
            || existing.identity != identity
        {
            return Err(conflict("Host removal destinations overlap incompatibly."));
        }
        existing.keys.extend(keys);
        existing.keys.sort();
        existing.keys.dedup();
        return Ok(());
    }
    let parent = super::files::directory_identity(path.parent().expect("file parent"))?.0;
    plan.edits.push(Edit {
        file: fileio::native(path),
        keys,
        before,
        after,
        mode,
        identity,
        parent,
        stage: uuid::Uuid::new_v4(),
    });
    Ok(())
}

pub(super) fn validate_edit(remover: &Remover, plan: &RemovalPlan, edit: &Edit) -> Result<()> {
    let path = fileio::path(&edit.file)?;
    let known = known(remover)?;
    if edit.keys.is_empty()
        || edit
            .keys
            .iter()
            .any(|key| known.get(key).is_none_or(|(p, _)| *p != path))
    {
        return Err(conflict(
            "Removal edit has unsupported component authority.",
        ));
    }
    let ledger = journal::Ledger {
        version: 1,
        roots: journal::roots(&remover.installer),
        entries: plan.ownership.clone(),
    };
    let expected = if edit.keys == ["codex.mcp"] {
        let mut document = toml::parse(&edit.before)?;
        let table = document
            .get_mut("mcp_servers")
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| conflict("Missing Codex removal selector."))?;
        let value = table
            .get("agentchat")
            .ok_or_else(|| conflict("Missing Codex removal member."))?;
        let value = toml::json(value)?;
        verify(
            &ledger,
            "codex.mcp",
            &path,
            known["codex.mcp"].1.clone(),
            value_digest(&value)?,
            recognize::mcp(&value, remover, false),
        )?;
        table.remove("agentchat");
        Some(document.to_string())
    } else if edit.keys == ["claude.mcp"] {
        let mut document = strict_json::parse(&edit.before)?;
        let table = document
            .get_mut("mcpServers")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| conflict("Missing Claude removal selector."))?;
        let value = table
            .get("agentchat")
            .ok_or_else(|| conflict("Missing Claude removal member."))?;
        verify(
            &ledger,
            "claude.mcp",
            &path,
            known["claude.mcp"].1.clone(),
            value_digest(value)?,
            recognize::mcp(value, remover, true),
        )?;
        table.remove("agentchat");
        Some(json_text(&document)?)
    } else if edit.keys.iter().all(|key| {
        matches!(
            key.as_str(),
            "claude.native.file-changed" | "claude.native.stop" | "claude.native.final"
        )
    }) {
        let mut document = strict_json::parse(&edit.before)?;
        let mut removed = super::hooks::clean(&mut document, remover, &ledger, &path)?;
        let mut keys = edit.keys.clone();
        removed.sort();
        keys.sort();
        if removed != keys {
            return Err(conflict(
                "Removal hook delta differs from its exact component set.",
            ));
        }
        Some(json_text(&document)?)
    } else {
        if edit.keys.iter().any(|key| known[key].1 != Selector::File) {
            return Err(conflict(
                "Whole-file removal cannot own shared configuration.",
            ));
        }
        let bundle = plan.edits.iter().any(|e| {
            e.keys.iter().any(|key| match key.as_str() {
                "claude.skill" => recognize::skill(&e.before),
                "claude.native.module" => recognize::native_module(&e.before, remover),
                "claude.native.manifest" => recognize::manifest(&e.before),
                _ => false,
            })
        });
        for key in &edit.keys {
            let recognizable = match key.as_str() {
                "codex.skill" | "claude.skill" => recognize::skill(&edit.before),
                "claude.native.module" => recognize::native_module(&edit.before, remover),
                "claude.native.input" => bundle && recognize::input(&edit.before),
                "claude.native.manifest" => recognize::manifest(&edit.before),
                "claude.native.modules" => bundle && recognize::modules(&edit.before),
                _ => false,
            };
            verify(
                &ledger,
                key,
                &path,
                Selector::File,
                super::super::digest(edit.before.as_bytes()),
                recognizable,
            )?;
        }
        None
    };
    if expected != edit.after {
        return Err(conflict(
            "Removal journal would change foreign configuration outside its selectors.",
        ));
    }
    Ok(())
}
