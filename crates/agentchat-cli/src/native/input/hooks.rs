//! Static stderr hands off new input; final guards continue native-owned queues.
mod permit;
use super::{Scope, required, state};
use crate::args::InputMode;
use agentchat_core::Result;
use agentchat_hosts::claude::{namespace::OwnerNamespace, observer::relay};
use agentchat_runtime::project::ProjectManager;
use permit::Permit;
use serde_json::Value;
use std::path::Path;
use tokio::io::AsyncWriteExt;

fn own(text: &str) -> bool {
    text.starts_with("<task-notification>") && text.contains("AGENTCHAT_INPUT_NOTICE root=")
}
pub(super) async fn run(executable: &Path, home: &Path, mode: InputMode) -> i32 {
    let final_hook = matches!(mode, InputMode::Final);
    let (bytes, oversized) = match relay::read_hook_input().await {
        Ok(v) => v,
        Err(_) => return if final_hook { 2 } else { 0 },
    };
    let raw_own = bytes
        .windows(b"AGENTCHAT_INPUT_NOTICE root=".len())
        .any(|w| w == b"AGENTCHAT_INPUT_NOTICE root=");
    if oversized {
        return if final_hook && raw_own { 2 } else { 0 };
    }
    let input: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return if final_hook && raw_own { 2 } else { 0 },
    };
    if final_hook && !own(input["prompt"].as_str().unwrap_or("")) {
        return 0;
    }
    match validate(executable, home, &input, final_hook).await {
        Ok(Some(text)) => {
            let mut stderr = tokio::io::stderr();
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                stderr.write_all(text.as_bytes()).await?;
                stderr.write_all(b"\n").await?;
                stderr.flush().await
            })
            .await;
            if matches!(result, Ok(Ok(()))) { 2 } else { 0 }
        }
        Ok(None) => 0,
        Err(_) => {
            if final_hook {
                2
            } else {
                0
            }
        }
    }
}
async fn validate(
    executable: &Path,
    home: &Path,
    input: &Value,
    final_hook: bool,
) -> Result<Option<String>> {
    let permits: Vec<Permit> = if final_hook {
        serde_json::from_value(input["agentchat_input_final_permits"].clone())
            .map_err(|_| required())?
    } else {
        vec![
            serde_json::from_value(input["agentchat_input_permit"].clone())
                .map_err(|_| required())?,
        ]
    };
    if permits.is_empty()
        || permits.len() > 32
        || permits
            .iter()
            .filter(|p| p.pending_proof().is_some())
            .count()
            > 1
    {
        return Err(required());
    }
    let owner = crate::native::owner(home, executable, false).await?;
    let manager = ProjectManager::new(home)?;
    let ns = OwnerNamespace::existing(manager.home(), owner).await?;
    for p in &permits {
        p.valid(input, final_hook)?;
        let scope = Scope {
            root: p.root,
            generation: p.generation,
        };
        let (_, native) = state::current(&ns, &scope).await?;
        if native.context.context_id != p.context_id || native.context.incarnation != p.incarnation
        {
            return Err(required());
        }
        if let Some(proof) = p.pending_proof() {
            let file = state::continuation(&manager, &ns, &scope, p.context_id, p.token).await?;
            if file.notice_proof()? != proof {
                return Err(required());
            }
        }
        if !final_hook {
            if input["hook_event_name"] != "Stop"
                && input["file_path"].as_str()
                    != native
                        .context
                        .working_directory
                        .join(".agentchat-wakeup")
                        .to_str()
            {
                return Err(required());
            }
            let file = state::load(&manager, &ns, &scope, Some(p.token))
                .await?
                .ok_or_else(required)?;
            if !file.input()?.addressed() {
                return Err(required());
            }
            return Ok(Some(file.notice()?));
        }
    }
    // Pending-head permits continue only the original still-current head;
    // accepted queue permits survive its retirement. Neither is a receipt or
    // permission to submit another body. Both bind complete original frames.
    permit::frames(input["prompt"].as_str().ok_or_else(required)?, &permits)?;
    Ok(None)
}
