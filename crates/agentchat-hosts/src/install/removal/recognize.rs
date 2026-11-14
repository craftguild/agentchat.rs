//! Lost-ledger authority requires exact product destinations and product content.
use super::Remover;
use crate::claude::{module, observer::NativeProfile};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

pub(super) fn mcp(value: &Value, remover: &Remover, claude: bool) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    let expected = if claude { 4 } else { 3 };
    if map.len() != expected
        || value["args"] != json!(["mcp"])
        || !value["command"].as_str().is_some_and(absolute)
        || claude && value["type"] != "stdio"
    {
        return false;
    }
    let Some(env) = value["env"].as_object() else {
        return false;
    };
    let native_prefix = if claude {
        "AGENTCHAT_CLAUDE_NATIVE"
    } else {
        "AGENTCHAT_CODEX_NATIVE"
    };
    let mut ordinary = env.clone();
    let managed_key = if claude {
        "AGENTCHAT_CLAUDE_MANAGED"
    } else {
        "AGENTCHAT_CODEX_MANAGED"
    };
    let managed = ordinary.remove(managed_key);
    let exe = ordinary.remove(&format!("{native_prefix}_EXE"));
    let version = ordinary.remove(&format!("{native_prefix}_VERSION"));
    // Managed dispatch and legacy fixed selection are disjoint exact shapes.
    // A mixed/unknown selector never grants lost-ledger deletion authority.
    let selected = match (managed, exe, version) {
        (None, None, None) => true,
        (Some(mode), None, None) => mode == "1",
        (None, Some(exe), Some(version)) => {
            exe.as_str().is_some_and(absolute) && version.as_str().is_some_and(version_label)
        }
        _ => false,
    };
    if !selected {
        return false;
    }
    environment(&Value::Object(ordinary), remover, claude)
}
fn environment(value: &Value, remover: &Remover, claude: bool) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    let Some(state) = remover.state().to_str() else {
        return false;
    };
    if map.get("AGENTCHAT_HOME").and_then(Value::as_str) != Some(state) {
        return false;
    }
    let mut expected = 1;
    if let Some(root) = &remover.environment.user_config_root {
        expected += 1;
        if !map
            .get("AGENTCHAT_USER_CONFIG_ROOT")
            .and_then(Value::as_str)
            .is_some_and(|p| same_root(Path::new(p), root))
        {
            return false;
        }
    } else if map.contains_key("AGENTCHAT_USER_CONFIG_ROOT") {
        return false;
    }
    let claude_dir = if claude && remover.environment.user_config_root.is_some() {
        remover.environment.claude_config_dir.as_ref().filter(|p| {
            p.starts_with(
                remover
                    .environment
                    .user_config_root
                    .as_ref()
                    .expect("selected root"),
            )
        })
    } else {
        None
    };
    if let Some(root) = claude_dir {
        expected += 1;
        if !map
            .get("CLAUDE_CONFIG_DIR")
            .and_then(Value::as_str)
            .is_some_and(|p| same_root(Path::new(p), root))
        {
            return false;
        }
    } else if map.contains_key("CLAUDE_CONFIG_DIR") {
        return false;
    }
    map.len() == expected
}
fn same_root(a: &Path, b: &Path) -> bool {
    super::super::fileio::absolute(a)
        .ok()
        .zip(super::super::fileio::absolute(b).ok())
        .is_some_and(|(a, b)| a == b)
}
pub(super) fn absolute(value: &str) -> bool {
    crate::setup::absolute(Path::new(value)).is_ok()
}
fn version_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

pub(super) fn native_module(text: &str, remover: &Remover) -> bool {
    let Some(line) = text
        .lines()
        .nth(2)
        .and_then(|s| s.strip_prefix("const AGENTCHAT = "))
    else {
        return false;
    };
    let Some((executable, rest)) = value(line) else {
        return false;
    };
    let Some((native, rest)) = rest.strip_prefix(", NATIVE = ").and_then(value) else {
        return false;
    };
    let Some((env, rest)) = rest.strip_prefix(", ENV = ").and_then(value) else {
        return false;
    };
    let version_source = if rest == ";" {
        text.lines()
            .nth(3)
            .and_then(|line| line.strip_prefix("let VERSION = "))
    } else {
        rest.strip_prefix(", VERSION = ")
    };
    let Some((version, rest)) = version_source.and_then(value) else {
        return false;
    };
    if rest != ";" || !environment(&env, remover, true) {
        return false;
    }
    let (Some(executable), Some(native), Some(version)) =
        (executable.as_str(), native.as_str(), version.as_str())
    else {
        return false;
    };
    if !absolute(executable) || !version_label(version) {
        return false;
    }
    let Ok(env) = serde_json::from_value::<BTreeMap<String, String>>(env) else {
        return false;
    };
    // Parsing locates values only. The complete canonical renderer remains the
    // ownership proof; edits to any other byte preserve the foreign component.
    let rendered = if native == crate::launch::MANAGED_CLAUDE {
        if version != "managed" {
            return false;
        }
        module::render_managed(Path::new(executable), &env)
    } else {
        if !absolute(native) {
            return false;
        }
        module::render(
            Path::new(executable),
            &NativeProfile {
                executable: native.into(),
                version: version.into(),
            },
            &env,
        )
    };
    rendered.is_ok_and(|expected| expected == text)
}
fn value(text: &str) -> Option<(Value, &str)> {
    let mut values = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let value = values.next()?.ok()?;
    Some((value, &text[values.byte_offset()..]))
}
pub(super) fn manifest(text: &str) -> bool {
    let Ok(value) = super::super::strict_json::parse(text) else {
        return false;
    };
    if value
        == json!({"name":"agentchat-native","version":"0.1.0",
        "description":"AgentChat managed Claude conversation controls"})
    {
        return true;
    }
    let Some(version) = value["description"]
        .as_str()
        .and_then(|s| s.strip_prefix("AgentChat native conversation controls for Claude Code "))
    else {
        return false;
    };
    version_label(version)
        && value
            == json!({"name":"agentchat-native","version":"0.1.0","description":format!("AgentChat native conversation controls for Claude Code {version}")})
}
pub(super) fn skill(text: &str) -> bool {
    text == include_str!("../../../assets/skills/agentchat/SKILL.md")
        // Frozen complete AgentChat skill assets: ADR0025 before the public
        // setup paragraph, and ADR0026's install-only intermediate paragraph.
        // A recognizable heading or product name alone never grants ownership.
        || ["5204659609bbb60bf17fb3387fddbde993f8b17a669ba5622855f01d836b69bb",
            "d7003b22c19cda58d0ccfc697ed304328e32e69d41fae3659a1a4884023279b4"]
            .contains(&super::super::digest(text.as_bytes()).as_str())
}
pub(super) fn input(text: &str) -> bool {
    text == module::input_source()
}
pub(super) fn modules(text: &str) -> bool {
    super::super::strict_json::parse(text).is_ok_and(|v| v == json!({"modules":["./register.js"]}))
}
