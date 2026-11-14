//! Real native candidate port for isolated G0; never the production admission gate.
#[path = "candidate_trace.rs"]
mod trace;
use super::protocol::{self, NativeProfile, Result};
use agentchat_core::{
    Error, ErrorCode, Result as CoreResult,
    delivery::{DeliveryPort, HostReceipt, MAX_WAKE_BYTES, Presentation, WakeDispatch},
    model::{Context, HostKind, Phase},
    ports::{HostGate, HostState},
    time::SystemClock,
};
use agentchat_hosts::claude::observer::NativeObserver;
use agentchat_runtime::{
    daemon::{RuntimeOptions, Server},
    project::ProjectManager,
};
use async_trait::async_trait;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncSeekExt, AsyncWriteExt, BufReader},
    sync::Mutex,
};

struct Candidate {
    observer: NativeObserver,
    root: PathBuf,
    generation: Mutex<u64>,
    timing: Arc<trace::Timing>,
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The exact candidate native boundary is unavailable.",
    )
}
#[async_trait]
impl HostGate for Candidate {
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::OriginalReceipt,
            ActiveInput::WithoutTurnId,
        ))
    }
    async fn verify(&self, context: &Context) -> CoreResult<HostState> {
        // This explicit test composition selects a real candidate, not an
        // admitted public profile. Every state comes from a new native nonce.
        // The ordinary executable still composes AdmittedHosts with no entries.
        if context.host != HostKind::Claude
            || context.host_version != "2.1.301"
            || context.os_profile != format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
            || context.working_directory != self.root.join("project")
        {
            return Err(unavailable());
        }
        self.observer.verify(context).await
    }
}
#[async_trait]
impl DeliveryPort for Candidate {
    async fn present(&self, context: &Context, dispatch: &WakeDispatch) -> CoreResult<HostReceipt> {
        let entered = Instant::now();
        protocol::audit(&self.root, self.timing.entry(dispatch, entered))
            .map_err(|_| unavailable())?;
        let state = self.verify(context).await?;
        if dispatch.hint.context_id != context.context_id
            || dispatch.hint.incarnation != context.incarnation
            || dispatch.hint.project_id != context.project_id
            || dispatch.hint.events.is_empty()
            || dispatch.hint.events.len() > 20
        {
            return Err(unavailable());
        }
        let expected_turn = match (&dispatch.presentation, state.phase) {
            (Presentation::Idle, Phase::Idle) => None,
            (
                Presentation::Active {
                    expected_turn_id: Some(turn),
                },
                Phase::Active,
            ) if state.active_turn_id.as_ref() == Some(turn) => Some(turn.clone()),
            _ => return Err(unavailable()),
        };
        if state.wake_paused {
            return Err(unavailable());
        }
        let hint = serde_json::to_vec(&dispatch.hint).map_err(|_| unavailable())?;
        if hint.len() + 512 > MAX_WAKE_BYTES {
            return Err(unavailable());
        }
        let token = dispatch.token;
        protocol::audit(&self.root, serde_json::json!({"event":"candidate_verified_boundary","token":token,"expected_turn_id":expected_turn,"phase":state.phase})).map_err(|_| unavailable())?;
        {
            // Serialize the shared in-place signal counter, while each exact
            // context has its own payload. Coalescing cannot retarget a hint.
            let mut generation = self.generation.lock().await;
            *generation = generation
                .checked_add(1)
                .filter(|g| *g <= 30)
                .ok_or_else(unavailable)?;
            let value = serde_json::json!({"root":context.host_session_id,"context_id":context.context_id,"incarnation":context.incarnation,"generation":*generation,"token":token,"expected_turn_id":expected_turn,"hint":dispatch.hint});
            protocol::write(
                &self.root.join(format!("wake-attempt-{token}.json")),
                &value,
            )
            .map_err(|_| unavailable())?;
            protocol::write(
                &self.root.join(format!("wake-{}.json", context.context_id)),
                &value,
            )
            .map_err(|_| unavailable())?;
            let path = self.root.join("project/.agentchat-wakeup");
            let m = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|_| unavailable())?;
            if !m.is_file() {
                return Err(unavailable());
            }
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .await
                .map_err(|_| unavailable())?;
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|_| unavailable())?;
            let text = format!("{}\n", *generation);
            file.write_all(text.as_bytes())
                .await
                .map_err(|_| unavailable())?;
            file.set_len(text.len() as u64)
                .await
                .map_err(|_| unavailable())?;
        }
        let signalled = Instant::now();
        let receipt = self.root.join(format!("submitted-{token}.json"));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if receipt.exists() {
                    let value: serde_json::Value =
                        protocol::read(&receipt).map_err(|_| unavailable())?;
                    if value["token"] != token.to_string()
                        || value["context_id"] != context.context_id.to_string()
                        || value["incarnation"] != context.incarnation.to_string()
                        || value["accepted"] != true
                    {
                        return Err(unavailable());
                    }
                    // A static command accepted this bounded context. Only an
                    // explicit model/tool ACK can acknowledge its deliveries.
                    protocol::audit(&self.root,serde_json::json!({"event":"candidate_native_accepted","token":token,"context_id":context.context_id,"port_entry_to_receipt_ms":entered.elapsed().as_secs_f64()*1000.0,"signal_write_to_receipt_ms":signalled.elapsed().as_secs_f64()*1000.0})).map_err(|_|unavailable())?;
                    return Ok(HostReceipt { turn_id: expected_turn.clone() });
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| unavailable())?
    }
}
pub(super) async fn run(root: &Path) -> Result<()> {
    let profile: NativeProfile = protocol::read(&root.join("native-profile.json"))?;
    if profile.version != "2.1.301" {
        return Err("unverified native candidate".into());
    }
    let manager = ProjectManager::from_environment()?;
    let project = manager
        .discover(&root.join("project"))
        .await?
        .ok_or("initialized candidate project required")?;
    let timing = Arc::new(trace::Timing::default());
    let port = Arc::new(Candidate {
        observer: NativeObserver::open(root.into(), profile).await?,
        root: root.into(),
        generation: Mutex::new(0),
        timing: timing.clone(),
    });
    let server = Server::bind(
        manager,
        project,
        port.clone(),
        Arc::new(SystemClock),
        RuntimeOptions::default(),
    )
    .await?
    .with_delivery(port)
    .with_commit_observer(timing);
    let handle = server.handle();
    protocol::write(
        &root.join("candidate-runtime-ready.json"),
        &serde_json::json!({
            "pid": std::process::id(), "instance": handle.instance_id(),
            "socket": handle.paths().socket, "candidate_verification_only": true
        }),
    )?;
    let serving = server.run();
    tokio::pin!(serving);
    let mut input = String::new();
    let mut stdin = BufReader::new(tokio::io::stdin());
    tokio::select! {
        result=&mut serving=>result?,
        _=stdin.read_line(&mut input)=>{handle.shutdown();serving.await?;},
    }
    Ok(())
}
