//! Validate all owned components before publishing any host configuration.
use super::{
    Artifact, Content, Installer, Selector, conflict, digest, fileio,
    journal::{self, Ledger, Owned, Plan, Write},
    strict_json, toml, value_digest,
};
use agentchat_core::Result;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};
use uuid::Uuid;

pub(super) fn prepare(installer: &Installer, artifacts: Vec<Artifact>) -> Result<Plan> {
    if artifacts.len() > 64 {
        return Err(conflict(
            "Installation exceeds sixty-four owned components.",
        ));
    }
    let (before_ledger, mut ledger) = journal::ledger(installer)?;
    let mut grouped = BTreeMap::<Vec<u8>, Vec<Artifact>>::new();
    let mut keys = HashSet::new();
    for mut artifact in artifacts {
        if !valid_key(&artifact.key) || !keys.insert(artifact.key.clone()) {
            return Err(conflict(
                "Installation component keys must be unique bounded identifiers.",
            ));
        }
        artifact.destination = destination(installer, &artifact.destination)?;
        grouped
            .entry(fileio::native(&artifact.destination))
            .or_default()
            .push(artifact);
    }
    if grouped.len() > 16 {
        return Err(conflict("Installation exceeds sixteen destination files."));
    }
    let mut writes = Vec::new();
    let mut total = 0usize;
    for (native, artifacts) in grouped {
        let path = fileio::path(&native)?;
        let before = fileio::read(&path, fileio::FILE_LIMIT)?;
        let after = document(&path, before.as_deref(), &artifacts, &mut ledger)?;
        if after.len() > fileio::FILE_LIMIT {
            return Err(conflict("Generated integration file exceeds one MiB."));
        }
        if before.as_deref() != Some(&after) {
            total += after.len() + before.as_ref().map_or(0, String::len);
            writes.push(Write {
                file: native,
                before,
                after,
                mode: fileio::mode(&path)?,
            });
        }
    }
    if total > 4 * fileio::FILE_LIMIT {
        return Err(conflict("Installation write set exceeds four MiB."));
    }
    ledger.validate(installer)?;
    let old = before_ledger
        .as_ref()
        .map(|text| strict_json::parse(text))
        .transpose()?;
    let ownership_changed = old.as_ref()
        != Some(&serde_json::to_value(&ledger).map_err(|_| conflict("Cannot encode ownership."))?);
    Ok(Plan {
        version: 1,
        id: Uuid::new_v4(),
        before_ledger,
        ledger,
        ownership_changed,
        writes,
    })
}
pub(super) fn destination(installer: &Installer, path: &Path) -> Result<std::path::PathBuf> {
    let resolved = if let Some((alias, root)) = installer
        .aliases
        .iter()
        .filter(|(alias, _)| path.starts_with(alias))
        .max_by_key(|(alias, _)| alias.components().count())
    {
        root.join(
            path.strip_prefix(alias)
                .map_err(|_| conflict("Invalid root alias."))?,
        )
    } else {
        path.to_owned()
    };
    journal::destination(installer, &resolved)?;
    Ok(resolved)
}
pub(super) fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
}
fn selector(content: &Content) -> Selector {
    match content {
        Content::File { .. } => Selector::File,
        Content::Json { path, .. } => Selector::Json { path: path.clone() },
        Content::JsonArray { path, .. } => Selector::JsonArray { path: path.clone() },
        Content::Toml { path, .. } => Selector::Toml { path: path.clone() },
    }
}
fn keys(selector: &Selector) -> Option<&[String]> {
    match selector {
        Selector::File => None,
        Selector::Json { path } | Selector::JsonArray { path } | Selector::Toml { path } => {
            Some(path)
        }
    }
}
fn owned<'a>(ledger: &'a Ledger, artifact: &Artifact) -> Result<Option<&'a str>> {
    let selected = selector(&artifact.content);
    if let Some(keys) = keys(&selected)
        && (keys.is_empty()
            || keys.len() > 8
            || keys
                .iter()
                .any(|s| s.is_empty() || s.len() > 128 || s.contains('\0')))
    {
        return Err(conflict("Invalid bounded configuration key path."));
    }
    let file = fileio::native(&artifact.destination);
    for (key, old) in &ledger.entries {
        if key == &artifact.key {
            if old.file != file || old.selector != selected {
                return Err(conflict(
                    "Owned component destination and selector are immutable.",
                ));
            }
        } else if old.file == file {
            let overlap = match (keys(&old.selector), keys(&selected)) {
                (Some(a), Some(b)) => a.starts_with(b) || b.starts_with(a),
                _ => true,
            };
            let shared_array = matches!((&old.selector,&selected),(Selector::JsonArray{path:a},Selector::JsonArray{path:b}) if a==b);
            if overlap && !shared_array {
                return Err(conflict("Owned integration selectors overlap."));
            }
        }
    }
    Ok(ledger.entries.get(&artifact.key).map(|o| o.hash.as_str()))
}
fn record(ledger: &mut Ledger, artifact: &Artifact, hash: String) {
    ledger.entries.insert(
        artifact.key.clone(),
        Owned {
            file: fileio::native(&artifact.destination),
            selector: selector(&artifact.content),
            hash,
        },
    );
}
fn document(
    path: &Path,
    before: Option<&str>,
    artifacts: &[Artifact],
    ledger: &mut Ledger,
) -> Result<String> {
    if let Content::File {
        text,
        exclusive_directory,
    } = &artifacts[0].content
    {
        if artifacts.len() != 1 {
            return Err(conflict(
                "A complete owned file cannot share a destination.",
            ));
        }
        let artifact = &artifacts[0];
        let old = owned(ledger, artifact)?;
        if old.is_none() && *exclusive_directory && path.parent().is_some_and(Path::exists) {
            return Err(conflict("Skill directory has an unowned name collision."));
        }
        if before.map(|s| digest(s.as_bytes())).as_deref() != old {
            return Err(conflict(
                "Owned file changed or has an unowned name collision.",
            ));
        }
        record(ledger, artifact, digest(text.as_bytes()));
        return Ok(text.clone());
    }
    if matches!(artifacts[0].content, Content::Toml { .. }) {
        let mut doc = toml::parse(before.unwrap_or(""))?;
        for artifact in artifacts {
            let Content::Toml { path, value } = &artifact.content else {
                return Err(conflict("Mixed integration document formats."));
            };
            let hash = toml::replace(&mut doc, path, value, owned(ledger, artifact)?)?;
            record(ledger, artifact, hash);
        }
        return Ok(doc.to_string());
    }
    let mut doc = strict_json::parse(before.unwrap_or("{}"))?;
    let original = doc.clone();
    if !doc.is_object() {
        return Err(conflict("Integration JSON must be an object."));
    }
    for artifact in artifacts {
        let (path, value, array) = match &artifact.content {
            Content::Json { path, value } => (path, value, false),
            Content::JsonArray { path, value } => (path, value, true),
            _ => return Err(conflict("Mixed integration document formats.")),
        };
        let old = owned(ledger, artifact)?;
        let hash = value_digest(value)?;
        let parent = parent(&mut doc, &path[..path.len() - 1])?;
        let key = &path[path.len() - 1];
        if array {
            let entries = parent
                .entry(key.clone())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| conflict("Hook destination is not an array."))?;
            let matches = entries
                .iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    value_digest(v)
                        .ok()
                        .filter(|h| Some(h.as_str()) == old || h == &hash)
                        .map(|_| i)
                })
                .collect::<Vec<_>>();
            match old {
                None if matches.is_empty() => entries.push(value.clone()),
                Some(old) if matches.len() == 1 && value_digest(&entries[matches[0]])? == old => {
                    entries[matches[0]] = value.clone()
                }
                _ => {
                    return Err(conflict(
                        "Hook member is missing, duplicated, modified or unowned.",
                    ));
                }
            }
        } else {
            let actual = parent.get(key).map(value_digest).transpose()?;
            if actual.as_deref() != old {
                return Err(conflict(
                    "JSON entry is missing, modified or has an unowned name collision.",
                ));
            }
            parent.insert(key.clone(), value.clone());
        }
        record(ledger, artifact, hash);
    }
    if doc == original
        && let Some(before) = before
    {
        return Ok(before.into());
    }
    let mut text =
        serde_json::to_string_pretty(&doc).map_err(|_| conflict("Cannot encode merged JSON."))?;
    text.push('\n');
    Ok(text)
}
fn parent<'a>(doc: &'a mut Value, path: &[String]) -> Result<&'a mut Map<String, Value>> {
    let mut current = doc;
    for key in path {
        current = current
            .as_object_mut()
            .ok_or_else(|| conflict("JSON path crosses a non-object value."))?
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    current
        .as_object_mut()
        .ok_or_else(|| conflict("JSON destination is not an object."))
}
