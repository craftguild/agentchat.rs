//! Actual executable fixtures; roots and host admission are explicitly isolated.

#![allow(dead_code)]

pub mod launch_bindings;
pub mod mcp;
use agentchat_core::{
    Result,
    model::{Context, HostKind, Phase},
    ports::{HostGate, HostState},
    time::SystemClock,
};
use agentchat_runtime::{
    context::{ContextRegistry, ContextSeed},
    daemon::{RuntimeHandle, RuntimeOptions, RuntimePaths, Server},
    project::{Project, ProjectManager},
};
use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Output, Stdio},
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{io::AsyncWriteExt, process::Command, task::JoinHandle};
use uuid::Uuid;

pub struct Fixture {
    pub root: TempDir,
    pub workspace: PathBuf,
    pub config: PathBuf,
    pub manager: ProjectManager,
}
impl Fixture {
    pub fn new() -> Self {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let config = root.path().join("config");
        std::fs::create_dir(&config).unwrap();
        let manager = ProjectManager::new(root.path().join("state")).unwrap();
        Self {
            root,
            workspace,
            config,
            manager,
        }
    }
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentchat"));
        command
            .current_dir(&self.workspace)
            .args(args)
            .env("AGENTCHAT_HOME", self.manager.home())
            .env("AGENTCHAT_USER_CONFIG_ROOT", &self.config)
            .env_remove("AGENTCHAT_CONTEXT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
    pub async fn raw(&self, args: &[&str], input: Option<Vec<u8>>) -> Output {
        let mut command = self.command(args);
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().unwrap();
        if let Some(input) = input {
            let mut stdin = child.stdin.take().unwrap();
            tokio::spawn(async move {
                let _ = stdin.write_all(&input).await;
            });
        }
        tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
            .await
            .unwrap()
            .unwrap()
    }
    pub async fn run(&self, args: &[&str]) -> (i32, Value) {
        self.input(args, None).await
    }
    pub async fn input(&self, args: &[&str], input: Option<Vec<u8>>) -> (i32, Value) {
        let mut arguments = vec!["--json"];
        arguments.extend_from_slice(args);
        let output = self.raw(&arguments, input).await;
        assert!(
            output.stderr.is_empty(),
            "unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "invalid CLI JSON {e}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        (output.status.code().unwrap(), value)
    }
    pub async fn initialize(&self) -> Project {
        self.manager
            .initialize(
                &self.workspace,
                agentchat_runtime::project::InitRequest::new(Uuid::new_v4()),
            )
            .await
            .unwrap()
            .project
    }
    pub async fn context(&self, session: &str) -> Context {
        ContextRegistry::new(self.manager.clone())
            .issue(ContextSeed {
                host: HostKind::Codex,
                host_session_id: session.into(),
                host_version: "fixture-only".into(),
                os_profile: "synthetic-not-admitted".into(),
                working_directory: self.workspace.clone(),
            })
            .await
            .unwrap()
    }
    pub async fn server(&self, project: &Project) -> (RuntimeHandle, JoinHandle<Result<()>>) {
        let server = Server::bind(
            self.manager.clone(),
            project.clone(),
            Arc::new(SyntheticHost),
            Arc::new(SystemClock),
            RuntimeOptions::default(),
        )
        .await
        .unwrap();
        (server.handle(), tokio::spawn(server.run()))
    }
    pub async fn stop_lazy(&self, project: &Project) {
        let socket = RuntimePaths::for_project(project).socket;
        std::fs::remove_file(project.reference_path()).unwrap();
        tokio::time::timeout(Duration::from_secs(4), async {
            while socket.exists() {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
    }
}
pub struct SyntheticHost;
#[async_trait::async_trait]
impl HostGate for SyntheticHost {
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::RetainUncertainty,
            ActiveInput::ExactTurnId,
        ))
    }
    fn allows_unmanaged_participation(&self) -> bool {
        true
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        assert_eq!(context.host_version, "fixture-only");
        assert_eq!(context.os_profile, "synthetic-not-admitted");
        Ok(HostState {
            phase: Phase::Idle,
            active_turn_id: None,
            wake_paused: false,
        })
    }
}

/// Synthetic consumer confirms only the exact complete input claimed by Core.
pub async fn accept_native(
    app: &agentchat_core::application::Application,
    context: &agentchat_core::model::Context,
    ids: Vec<Uuid>,
) {
    let mut remaining: std::collections::HashSet<_> = ids.into_iter().collect();
    for _ in 0..1_000 {
        if remaining.is_empty() {
            return;
        }
        let dispatch = app
            .prepare_native(context)
            .await
            .unwrap()
            .expect("eligible complete input");
        assert!(
            !dispatch.awaiting_acceptance(),
            "this consumer cannot invent an original recovery result"
        );
        let input = dispatch.input();
        assert_eq!(
            agentchat_core::delivery::NativeInput::from_text(&input.text().unwrap()).unwrap(),
            input
        );
        remaining.remove(&dispatch.delivery_id());
        assert!(
            app.finish_native(
                context,
                &dispatch,
                Ok(agentchat_core::delivery::MessageProgress::Accepted(
                    agentchat_core::delivery::HostReceipt::default()
                ))
            )
            .await
            .unwrap()
        );
    }
    panic!("bounded synthetic input consumer did not reach expected deliveries");
}
