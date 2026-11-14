//! Plan and install with the same production installer and embedded assets.
use super::Result;
use agentchat_core::model::HostKind;
use agentchat_hosts::{
    claude::observer::NativeProfile,
    install::{Content, Installer},
    setup::Environment,
};
use serde_json::{Map, json};
use std::path::PathBuf;

pub(super) async fn run(
    root: PathBuf,
    config: PathBuf,
    native: PathBuf,
    executable: Option<PathBuf>,
    version: String,
    apply: bool,
) -> Result<()> {
    // Explicit sandbox inputs prevent ambient HOME/CODEX_HOME from selecting
    // installation targets. Credentials and unrelated host values are not read.
    let roots = Environment {
        user_config_root: Some(config.clone()),
        claude_config_dir: Some(config.join("host-config")),
        ..Environment::default()
    }
    .resolve()?;
    let state = root.join("state");
    let installer = Installer::new(
        &state,
        roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
    )?;
    let expected_native = NativeProfile {
        executable: native,
        version,
    };
    if !expected_native.executable.is_absolute()
        || !agentchat_hosts::claude::module::candidate_version(&expected_native.version)
    {
        return Err("The acceptance fixture requires an explicit candidate observation".into());
    }
    // Reproducible validation inputs stay in this report, not in the owned
    // dispatcher. Actual startup independently resolves PATH and checks that
    // its selected image/version agrees with the explicit fixture observation.
    let plan = roots.claude_managed_artifacts(
        &executable.unwrap_or_else(|| root.join("bin/agentchat")),
        &state,
    )?;
    let (first, repeated, health) = if apply {
        (
            Some(installer.install(plan.clone()).await?),
            Some(installer.install(plan.clone()).await?),
            Some(installer.inspect(plan.clone()).await?),
        )
    } else {
        (None, None, None)
    };
    let mut hooks = Map::new();
    let mut mcp = None;
    for artifact in plan {
        match artifact.content {
            Content::JsonArray { path, value } => {
                hooks.insert(path.last().ok_or("Hook path is empty")?.clone(), value);
            }
            Content::Json { value, .. } if artifact.key == "claude.mcp" => mcp = Some(value),
            _ => {}
        }
    }
    println!(
        "{}",
        json!({"first": first, "repeated": repeated, "health": health,
        "hooks": hooks, "mcp": mcp, "expected_native": expected_native,
        "managed_dispatch": true, "admitted": false})
    );
    Ok(())
}
