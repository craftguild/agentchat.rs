//! Static/final hooks fail closed for owned notices and remain inert otherwise.
mod permit;
use crate::{args::GuardMode, claude_gateway::records};
use agentchat_core::{
    Result,
    delivery::{MAX_WAKE_BYTES, MAX_WAKE_EVENTS},
};
use agentchat_hosts::claude::{
    NativeLifecycle,
    namespace::OwnerNamespace,
    observer::relay,
    wake::{Submission, WakeRecord},
};
use permit::{Permit, required};
use serde_json::Value;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub(super) async fn run(executable: &Path, state: &Path, mode: GuardMode) -> i32 {
    let (bytes, oversized) = match relay::read_hook_input().await {
        Ok(input) => input,
        Err(_) => return 2,
    };
    let raw_own = bytes
        .windows(b"AGENTCHAT_TOKEN_NOTICE".len())
        .any(|w| w == b"AGENTCHAT_TOKEN_NOTICE");
    if oversized {
        return if matches!(mode, GuardMode::Final) && raw_own {
            2
        } else {
            0
        };
    }
    let input: Value = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(_) => {
            return if matches!(mode, GuardMode::Final) && raw_own {
                2
            } else {
                0
            };
        }
    };
    let text = input.get("prompt").and_then(Value::as_str).unwrap_or("");
    let own = text.contains("AGENTCHAT_TOKEN_NOTICE");
    if matches!(mode, GuardMode::Final) && !own {
        return 0;
    }
    let field = match mode {
        GuardMode::Notice => "agentchat_guard_permit",
        GuardMode::Final => "agentchat_guard_final_permit",
    };
    let permit: Permit = match input.get(field).cloned().map(serde_json::from_value) {
        Some(Ok(p)) => p,
        _ => {
            return if matches!(mode, GuardMode::Final) {
                2
            } else {
                0
            };
        }
    };
    let root = input
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let now = match SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| v.as_millis().try_into().ok())
    {
        Some(now) => now,
        None => return 2,
    };
    if !root.is_some_and(|root| permit.valid(now, root))
        || (matches!(mode, GuardMode::Final)
            && (permit.phase.as_deref() != Some("final")
                || permit.epoch.is_none()
                || !permit.matches_notice(text)))
    {
        return if matches!(mode, GuardMode::Final) {
            2
        } else {
            0
        };
    }
    let changed = input
        .get("file_path")
        .and_then(Value::as_str)
        .map(Path::new);
    match validate(executable, state, mode, permit, now, changed).await {
        Ok(Some(notice)) => {
            let mut stderr = tokio::io::stderr();
            if stderr.write_all(notice.as_bytes()).await.is_err()
                || stderr.write_all(b"\n").await.is_err()
            {
                return 0;
            }
            2
        }
        Ok(None) => 0,
        Err(_) => {
            if matches!(mode, GuardMode::Final) {
                2
            } else {
                0
            }
        }
    }
}
async fn validate(
    executable: &Path,
    state: &Path,
    mode: GuardMode,
    permit: Permit,
    now: u64,
    changed: Option<&Path>,
) -> Result<Option<String>> {
    let owner = crate::native::owner(state, executable, false).await?;
    let manager = agentchat_runtime::project::ProjectManager::new(state)?;
    let namespace = OwnerNamespace::existing(manager.home(), owner).await?;
    let current = namespace.read_current().await?.ok_or_else(required)?;
    let NativeLifecycle::Project(registration) = &current.lifecycle else {
        return Err(required());
    };
    let context = &registration.context;
    if registration.root != permit.root
        || registration.generation != permit.native_generation
        || context.context_id != permit.context_id
        || context.incarnation != permit.incarnation
        || !agentchat_hosts::claude::module::candidate_version(&context.host_version)
        || (matches!(mode, GuardMode::Notice)
            && changed
                != Some(
                    context
                        .working_directory
                        .join(".agentchat-wakeup")
                        .as_path(),
                ))
        || current.reservation.transport_nonce
            != Some(namespace.transport().await?.ok_or_else(required)?.nonce)
    {
        return Err(required());
    }
    let path = namespace.directory().to_owned();
    let endpoint_path = records::endpoint(&path, namespace.id());
    let endpoint_nonce = current.reservation.transport_nonce.ok_or_else(required)?;
    let context = context.clone();
    let wake_path = path.join(format!("wake-{}.json", context.context_id));
    let first_path = wake_path.clone();
    let wake: WakeRecord = tokio::task::spawn_blocking(move || records::read(&first_path))
        .await
        .map_err(|_| required())??;
    // Never await RPC under the owner cursor lock. The independent application
    // snapshot invalidates stale file hints after leave, replacement, complete-input settlement or
    // failed native submission; it cannot call back into this native hook.
    let project = manager
        .discover(&context.working_directory)
        .await?
        .ok_or_else(required)?;
    if project.identity().id != context.project_id {
        return Err(required());
    }
    let paths = agentchat_runtime::daemon::RuntimePaths::for_project(&project);
    let valid = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let mut client =
            agentchat_protocol::rpc::Client::connect(paths.socket, context.project_id).await?;
        agentchat_protocol::rpc::success(
            client
                .call(
                    Uuid::new_v4(),
                    agentchat_protocol::rpc::Call::CheckDelivery {
                        context_id: context.context_id,
                        batch_id: wake.hint.batch_id,
                        token: permit.dispatch,
                        delivery_ids: wake
                            .hint
                            .events
                            .iter()
                            .map(|event| event.delivery_id)
                            .collect(),
                    },
                )
                .await?,
        )
    })
    .await
    .map_err(|_| required())??;
    if valid["current"] != true {
        return Err(required());
    }
    // Hold the current-intent fence across reading the wake and publishing its
    // bounded receipt. A delayed predecessor cannot publish for its successor.
    let publisher = namespace.metadata_publisher()?;
    namespace
        .publish_current(current.reservation, move |_| {
            let endpoint:records::Endpoint=records::read(&endpoint_path).map_err(|_|required())?;
            let latest:WakeRecord=records::read(&wake_path).map_err(|_|required())?;
            let fresh_now=SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|d|d.as_millis().try_into().ok()).ok_or_else(required)?;
            if endpoint.nonce!=endpoint_nonce || latest!=wake || !permit.valid(fresh_now,permit.root) || wake.root != permit.root
                || wake.context_id != permit.context_id
                || wake.incarnation != permit.incarnation
                || wake.token != permit.dispatch
                || wake.generation != permit.generation
                || wake.expected_turn_id != permit.expected_turn_id
                || wake.hint.version != 1
                || wake.hint.context_id != context.context_id
                || wake.hint.incarnation != context.incarnation
                || wake.hint.project_id != context.project_id
                || wake.hint.events.is_empty()
                || wake.hint.events.len() > MAX_WAKE_EVENTS
            {
                return Err(required());
            }
            let receipt_path = path.join(format!("submitted-{}.json", context.context_id));
            if matches!(mode, GuardMode::Final) {
                return Ok(None);
            }
            if receipt_path.try_exists().map_err(|_| required())? {
                let previous: Submission = records::read(&receipt_path).map_err(|_| required())?;
                if previous.accepted
                    && previous.token == permit.dispatch
                    && previous.context_id == permit.context_id
                    && previous.incarnation == permit.incarnation
                {
                    return Ok(None);
                }
            }
            let hint = serde_json::to_string(&wake.hint).map_err(|_| required())?;
            let text = format!(
                "AGENTCHAT_TOKEN_NOTICE root={} generation={} token={}. AgentChat notification. Use AgentChat tools and the shared agentchat skill. Hint JSON: {}",
                permit.root, permit.generation, permit.token, hint
            );
            if text.len() > MAX_WAKE_BYTES {
                return Err(required());
            }
            publisher.write(&receipt_path, &Submission {
                accepted: true,
                token: permit.dispatch,
                context_id: permit.context_id,
                incarnation: permit.incarnation,
                root: permit.root,
                generation: permit.generation,
                unix_ms: now,
            }).map_err(|_| required())?;
            Ok(Some(text))
        })
        .await
}
