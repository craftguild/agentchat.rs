//! Render native code for one bounded observed engine identity label.
use super::observer::NativeProfile;
use agentchat_core::{Error, ErrorCode, Result};
use std::{collections::BTreeMap, path::Path};

/// Render the native callback module with literal argv/environment
/// values. No shell expansion, host execution, trust approval, configuration
/// write or compatibility admission occurs. Real native acceptance is separate.
pub fn render(
    executable: &Path,
    profile: &NativeProfile,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    if !candidate_version(&profile.version) {
        return Err(Error::new(
            ErrorCode::UnsupportedHost,
            "The native module engine version is unsupported.",
        ));
    }
    let native = crate::setup::encoded(&profile.executable)?;
    render_values(executable, native, &profile.version, environment)
}

/// Render a stable dispatcher. The actual version is obtained from the exact
/// original managed snapshot and compared with the live native session getter.
pub fn render_managed(executable: &Path, environment: &BTreeMap<String, String>) -> Result<String> {
    render_values(
        executable,
        crate::launch::MANAGED_CLAUDE,
        "managed",
        environment,
    )
}

fn render_values(
    executable: &Path,
    native: &str,
    version: &str,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    let executable = crate::setup::encoded(executable)?;
    if !environment.contains_key("AGENTCHAT_HOME") || environment.len() > 3 {
        return Err(invalid());
    }
    for (key, value) in environment {
        if !matches!(
            key.as_str(),
            "AGENTCHAT_HOME" | "AGENTCHAT_USER_CONFIG_ROOT" | "CLAUDE_CONFIG_DIR"
        ) {
            return Err(invalid());
        }
        crate::setup::encoded(Path::new(value))?;
    }
    let mut values = vec![
        (
            "__AGENTCHAT_EXECUTABLE__",
            serde_json::to_string(executable).map_err(|_| invalid())?,
        ),
        (
            "__AGENTCHAT_NATIVE_EXECUTABLE__",
            serde_json::to_string(native).map_err(|_| invalid())?,
        ),
        (
            "__AGENTCHAT_ENVIRONMENT__",
            serde_json::to_string(environment).map_err(|_| invalid())?,
        ),
    ];
    values.push((
        "__AGENTCHAT_VERSION__",
        serde_json::to_string(version).map_err(|_| invalid())?,
    ));
    let source = include_str!("../../assets/claude/2.1.302/register.js");
    let mut replacements = Vec::new();
    for (token, value) in values {
        let positions = source
            .match_indices(token)
            .map(|(at, _)| at)
            .collect::<Vec<_>>();
        if positions.len() != 1 {
            return Err(invalid());
        }
        replacements.push((positions[0], token, value));
    }
    replacements.sort_by_key(|(at, _, _)| *at);
    let mut rendered = String::new();
    let mut offset = 0;
    // Replace only original template spans. A literal path containing another
    // placeholder must never be interpreted as another substitution.
    for (at, token, value) in replacements {
        rendered.push_str(&source[offset..at]);
        rendered.push_str(&value);
        offset = at + token.len();
    }
    rendered.push_str(&source[offset..]);
    Ok(rendered)
}
fn invalid() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        "The native module paths or environment are invalid.",
    )
}

/// Whether a bounded observed native identity label can select the adapter.
///
/// The label is not a compatibility or admission decision. Live SDK, selected
/// executable and application-context equality checks remain separate fences.
pub fn candidate_version(version: &str) -> bool {
    !version.is_empty() && version.len() <= 128 && !version.chars().any(char::is_control)
}
/// Whether the selected adapter includes the first-input gate.
///
/// Every valid observed label uses the current full-input adapter. This remains
/// source feature selection only, never native compatibility or admission proof.
pub fn has_startup_gate_source(version: &str) -> bool {
    candidate_version(version)
}
/// Unconfigured full-input SDK companion. Install beside its identity-labelled
/// entrypoint; source inclusion neither executes it nor admits the profile.
pub fn input_source() -> &'static str {
    include_str!("../../assets/claude/2.1.302/input.js")
}
