//! Explicit native MCP setup; missing per-call proof never selects bootstrap.
use super::Configuration;
use agentchat_core::Result;
use agentchat_hosts::claude::{
    namespace::OwnerNamespace, observer::NativeProfile, process::ProcessIdentity,
};
use agentchat_runtime::project::ProjectManager;
use std::path::Path;

/// The installer supplies the native executable for this host's MCP entry.
/// Generic MCP has no such selection and remains entirely unbound.
pub(crate) async fn configuration(
    manager: &ProjectManager,
    executable: &Path,
) -> Result<Configuration> {
    let (owner, profile) = if executable == Path::new(agentchat_hosts::launch::MANAGED_CLAUDE) {
        agentchat_hosts::launch::managed_claude(manager.home(), false).await?
    } else {
        let owner = ProcessIdentity::ancestor(executable)?;
        let profile = NativeProfile {
            executable: owner.executable(),
            version: version()?,
        };
        (owner, profile)
    };
    let namespace = OwnerNamespace::open(manager.home(), owner.clone()).await?;
    namespace.publish_profile(profile.clone()).await?;
    Ok(Configuration {
        directory: namespace.directory().into(),
        profile,
        owner_hint: namespace.id(),
        owner_cursor: Some(namespace),
        tool_prefix: "mcp__agentchat__".into(),
    })
}
/// Native direct-child helpers and MCP ancestry derive the same physical scope.
/// This command returns no application/conversation identity or wake permission.
pub(crate) async fn namespace(
    manager: &ProjectManager,
    executable: &Path,
    version: &str,
) -> Result<serde_json::Value> {
    let (owner, profile) = if executable == Path::new(agentchat_hosts::launch::MANAGED_CLAUDE) {
        agentchat_hosts::launch::managed_claude(manager.home(), true).await?
    } else {
        let owner = ProcessIdentity::parent(executable)?;
        let profile = NativeProfile {
            executable: owner.executable(),
            version: version.into(),
        };
        (owner, profile)
    };
    let namespace = OwnerNamespace::open(manager.home(), owner.clone()).await?;
    namespace.publish_profile(profile.clone()).await?;
    Ok(
        serde_json::json!({"directory":namespace.directory(),"profile":profile,
        "owner_root":namespace.id(),"observer":namespace.observer(),
        "generation_floor":namespace.generation_floor().await?}),
    )
}

fn version() -> Result<String> {
    let version = std::env::var("AGENTCHAT_CLAUDE_NATIVE_VERSION").map_err(|_| {
        agentchat_core::Error::new(
            agentchat_core::ErrorCode::ConfigurationConflict,
            "The configured native engine has no observed version identity.",
        )
    })?;
    if !agentchat_hosts::claude::module::candidate_version(&version) {
        return Err(agentchat_core::Error::new(
            agentchat_core::ErrorCode::UnsupportedHost,
            "The configured native engine has no adapter candidate.",
        ));
    }
    Ok(version)
}
