//! Native-only full input callbacks; publication and acceptance stay distinct.
mod hooks;
mod state;
use crate::args::InputMode;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::claude::{
    body::NativeInputResult, namespace::OwnerNamespace, observer::relay,
};
use agentchat_runtime::project::ProjectManager;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scope {
    pub root: Uuid,
    pub generation: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    root: Uuid,
    generation: u64,
    context_id: Uuid,
    token: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    root: Uuid,
    generation: u64,
    context_id: Uuid,
    token: Uuid,
    native: NativeInputResult,
}
pub(super) fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original native full-input boundary is unavailable.",
    )
}
pub(super) async fn run(executable: &Path, home: &Path, mode: InputMode) -> i32 {
    if matches!(mode, InputMode::Notice | InputMode::Final) {
        return hooks::run(executable, home, mode).await;
    }
    match serve(executable, home, mode).await {
        Ok(value) => match output(&value).await {
            Ok(()) => 0,
            Err(error) => crate::mcp::failure(error).await,
        },
        Err(error) => crate::mcp::failure(error).await,
    }
}
async fn serve(executable: &Path, home: &Path, mode: InputMode) -> Result<Value> {
    // A model-launched shell or surviving helper cannot claim to be the direct
    // SDK producer. No namespace is created by this observation/receipt command.
    let owner = crate::native::owner(home, executable, true).await?;
    let manager = ProjectManager::new(home)?;
    let ns = OwnerNamespace::existing(manager.home(), owner).await?;
    // Native row results include the whole body. Keep the ordinary 64-KiB
    // control protocol unchanged and use the separate bounded hook reader.
    let (bytes, oversized) = relay::read_hook_input().await?;
    if oversized {
        return Err(required());
    }
    match mode {
        InputMode::Load => {
            let scope: Scope = serde_json::from_slice(&bytes).map_err(|_| required())?;
            match state::load(&manager, &ns, &scope, None).await? {
                Some(file) => {
                    let (digest, bytes) = file.notice_proof()?;
                    Ok(
                        json!({"addressed":file.input()?.addressed(),"proof":{"digest":digest,"bytes":bytes},"file":file}),
                    )
                }
                None => Ok(json!({"file":null})),
            }
        }
        InputMode::Claim => {
            let claim: Claim = serde_json::from_slice(&bytes).map_err(|_| required())?;
            let claimed = state::claim(
                &manager,
                &ns,
                &Scope {
                    root: claim.root,
                    generation: claim.generation,
                },
                claim.context_id,
                claim.token,
            )
            .await?;
            Ok(json!({"claimed":claimed}))
        }
        InputMode::Record => {
            let record: Record = serde_json::from_slice(&bytes).map_err(|_| required())?;
            state::record(
                &ns,
                Scope {
                    root: record.root,
                    generation: record.generation,
                },
                record.context_id,
                record.token,
                record.native,
            )
            .await?;
            Ok(json!({"recorded":true}))
        }
        _ => unreachable!("static hooks use their own exit semantics"),
    }
}
async fn output(value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| required())?;
    if bytes.len() > 1_048_576 {
        return Err(required());
    }
    bytes.push(b'\n');
    let mut output = relay::native_output()?;
    tokio::time::timeout(Duration::from_secs(2), async {
        output.write_all(&bytes).await?;
        output.flush().await
    })
    .await
    .map_err(|_| required())?
    .map_err(|_| required())
}
