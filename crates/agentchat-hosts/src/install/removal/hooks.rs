//! Remove only exact supported AgentChat hook members from mixed native groups.
use super::super::{Selector, conflict, fileio, journal, value_digest};
use super::{Remover, component};
use agentchat_core::Result;
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn clean(
    document: &mut Value,
    remover: &Remover,
    ledger: &journal::Ledger,
    path: &Path,
) -> Result<Vec<String>> {
    let Some(hooks) = document.get_mut("hooks") else {
        return Ok(Vec::new());
    };
    let hooks = hooks.as_object_mut().ok_or_else(|| {
        component(
            conflict("Native hooks settings are not an object."),
            "claude.hooks",
            path,
        )
    })?;
    let mut removed = Vec::new();
    for (event, key, mode) in [
        ("FileChanged", "claude.native.file-changed", "notice"),
        ("Stop", "claude.native.stop", "notice"),
        ("UserPromptSubmit", "claude.native.final", "final"),
    ] {
        let Some(groups) = hooks.get_mut(event) else {
            continue;
        };
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| component(conflict("Native hook event is not an array."), key, path))?;
        let mut next = Vec::new();
        let mut changed = false;
        for group in groups.iter() {
            let owned = ledger.entries.get(key).is_some_and(|record| {
                record.file == fileio::native(path)
                    && record.selector
                        == Selector::JsonArray {
                            path: vec!["hooks".into(), event.into()],
                        }
                    && value_digest(group).ok().as_deref() == Some(record.hash.as_str())
            });
            if owned {
                changed = true;
                continue;
            }
            let Some(members) = group.get("hooks").and_then(Value::as_array) else {
                next.push(group.clone());
                continue;
            };
            let mut kept = Vec::new();
            let mut changed_group = false;
            for member in members {
                match recognized(member, remover, mode) {
                    Match::Owned => {
                        changed = true;
                        changed_group = true;
                    }
                    Match::Foreign => kept.push(member.clone()),
                    Match::Ambiguous => {
                        return Err(component(
                            conflict(
                                "A native hook has ambiguous AgentChat command arguments or metadata; it was preserved.",
                            ),
                            key,
                            path,
                        ));
                    }
                }
            }
            if !changed_group {
                next.push(group.clone());
                continue;
            }
            let ordinary_group = if event == "FileChanged" {
                group.as_object().is_some_and(|g| {
                    g.len() == 2 && g.get("matcher") == Some(&json!(".agentchat-wakeup"))
                })
            } else {
                group.as_object().is_some_and(|g| g.len() == 1)
            };
            if kept.is_empty() && ordinary_group {
                continue;
            }
            let mut retained = group.clone();
            retained["hooks"] = Value::Array(kept);
            next.push(retained);
        }
        if changed {
            *groups = next;
            removed.push(key.into());
        }
    }
    Ok(removed)
}
enum Match {
    Owned,
    Foreign,
    Ambiguous,
}
fn recognized(member: &Value, remover: &Remover, mode: &str) -> Match {
    let Some(command) = member.get("command").and_then(Value::as_str) else {
        return Match::Foreign;
    };
    let looks_specific =
        command.contains(" internal claude-input ") || command.contains(" internal claude-guard ");
    let Some((executable, rest)) = quoted(command) else {
        return if looks_specific {
            Match::Ambiguous
        } else {
            Match::Foreign
        };
    };
    let managed_input = rest.starts_with(" internal claude-input ");
    let Some(rest) = rest
        .strip_prefix(" internal claude-input ")
        .or_else(|| rest.strip_prefix(" internal claude-guard "))
    else {
        return Match::Foreign;
    };
    let Some(rest) = rest.strip_prefix(&format!("--mode {mode} --state-directory ")) else {
        return Match::Ambiguous;
    };
    let Some((state, rest)) = quoted(rest) else {
        return Match::Ambiguous;
    };
    if Some(state.as_str()) != remover.state().to_str() {
        // Old ac and independent AgentChat roots can share these internal
        // command words. Positive selection of another valid absolute state
        // root is outside this cleanup, never executable-basename ownership.
        let path = Path::new(&state);
        if !super::recognize::absolute(&state) || fileio::safe_parents(&path.join("entry")).is_err()
        {
            return Match::Ambiguous;
        }
        let Ok(resolved) = fileio::absolute(path) else {
            return Match::Ambiguous;
        };
        if resolved != remover.state() {
            return Match::Foreign;
        }
    }
    let Some(rest) = rest.strip_prefix(" --native-executable ") else {
        return Match::Ambiguous;
    };
    let Some((native, rest)) = quoted(rest) else {
        return Match::Ambiguous;
    };
    if !rest.is_empty()
        || !super::recognize::absolute(&executable)
        || !(super::recognize::absolute(&native)
            || managed_input && native == crate::launch::MANAGED_CLAUDE)
    {
        return Match::Ambiguous;
    }
    let expected = if mode == "notice" {
        json!({"type":"command","command":command,"asyncRewake":true,"timeout":15})
    } else {
        json!({"type":"command","command":command,"timeout":10,"onFailure":"block"})
    };
    if *member == expected {
        Match::Owned
    } else {
        Match::Ambiguous
    }
}
fn quoted(text: &str) -> Option<(String, &str)> {
    let mut rest = text.strip_prefix('\'')?;
    let mut value = String::new();
    loop {
        let end = rest.find('\'')?;
        value.push_str(&rest[..end]);
        rest = &rest[end..];
        if let Some(next) = rest.strip_prefix("'\"'\"'") {
            value.push('\'');
            rest = next;
        } else {
            return Some((value, &rest[1..]));
        }
    }
}
