//! First input waits only for original post-commit startup, outside cursor locks.
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::{
    claude::observer::relay,
    launch::{Registry, StartupScope, StartupState},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Facts {
    root: Uuid,
    cwd: PathBuf,
    version: String,
}

pub(super) async fn run(executable: &Path) -> Result<Value> {
    let manager = agentchat_runtime::project::ProjectManager::from_environment()?;
    let owner = crate::native::owner(manager.home(), executable, true).await?;
    let scope = StartupScope::from_environment()?;
    let facts: Facts = serde_json::from_slice(&relay::read_input(65_536).await?)
        .map_err(|_| Error::new(ErrorCode::InvalidArgument, "Invalid native startup facts."))?;
    if facts.root.is_nil()
        || !facts.cwd.is_absolute()
        || !agentchat_hosts::claude::module::candidate_version(&facts.version)
    {
        return Err(Error::new(
            ErrorCode::SessionRequired,
            "Exact native startup facts are required.",
        ));
    }
    let Some(scope) = scope else {
        return Ok(json!({"state":"unmanaged","ready":true,"cacheable":true}));
    };
    let registry = Registry::new(manager.home());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state =
            registry.startup_state(&scope, &owner, facts.root, &facts.cwd, &facts.version)?;
        if state != StartupState::Waiting || tokio::time::Instant::now() >= deadline {
            return Ok(json!({"state":state,"ready":state!=StartupState::Waiting,
                "cacheable":matches!(state,StartupState::Released|StartupState::OtherOwner)}));
        }
        // One existing native callback holds its original input. This bounded
        // local read neither acquires the owner cursor nor calls the runtime or
        // native API, so the supervisor may independently finish startup proof.
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
