//! Fixed-clock shared-operation parity with test-owned private native contexts.
use crate::support::{Fixture, SyntheticHost, mcp::Mcp};
use agentchat_core::time::{Clock, SystemClock, Timestamp};
use agentchat_core::{ErrorCode, application::Mutation};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::daemon::{RuntimeHandle, RuntimeOptions, Server};
use serde_json::{Value, json};
use std::{collections::BTreeSet, process::Stdio, sync::Arc};
use tokio::task::JoinHandle;
use uuid::Uuid;

struct Fixed(Timestamp);
impl Clock for Fixed {
    fn now(&self) -> Timestamp {
        self.0
    }
}
pub struct Rig {
    pub f: Fixture,
    pub peers: Vec<Mcp>,
    pub contexts: [Uuid; 2],
    pub project_id: Uuid,
    bound: [bool; 2],
    pub covered: BTreeSet<String>,
    pub errors: BTreeSet<String>,
    pub runtime: RuntimeHandle,
    task: Option<JoinHandle<agentchat_core::Result<()>>>,
}
impl Rig {
    pub async fn new() -> Self {
        let f = Fixture::new();
        let mut a = Self::peer(&f).await;
        let id = Uuid::new_v4();
        let initialized = a
            .call(
                "agentchat_init",
                json!({"project_path":f.workspace,"scope":"directory","request_id":id}),
            )
            .await;
        assert!(initialized.get("result").is_some(), "{initialized}");
        let (code, cli) = Self::cli_raw(
            &f,
            None,
            &["install".into(), "--scope".into(), "directory".into()],
            id,
            None,
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(initialized, cli);
        assert_eq!(cli["result"]["created"], true);
        let project = f.manager.discover(&f.workspace).await.unwrap().unwrap();
        let project_id = project.identity().id;
        let contexts = [
            f.context("parity-alice").await.context_id,
            f.context("parity-bob").await.context_id,
        ];
        let server = Server::bind(
            f.manager.clone(),
            project,
            Arc::new(SyntheticHost),
            Arc::new(Fixed(SystemClock.now())),
            RuntimeOptions::default(),
        )
        .await
        .unwrap();
        let runtime = server.handle();
        let task = tokio::spawn(server.run());
        let b = Self::peer(&f).await;
        Self {
            f,
            peers: vec![a, b],
            contexts,
            project_id,
            bound: [false; 2],
            covered: BTreeSet::from(["agentchat_init".into()]),
            errors: BTreeSet::new(),
            runtime,
            task: Some(task),
        }
    }
    async fn peer(f: &Fixture) -> Mcp {
        // Empty PATH makes doctor deterministic without invoking real host CLIs.
        // The test project explicitly uses directory scope outside any Git repo.
        Mcp::from_child(
            f.command(&["mcp"])
                .env("PATH", "")
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        )
        .await
    }
    async fn cli_raw(
        f: &Fixture,
        context: Option<Uuid>,
        command: &[String],
        id: Uuid,
        input: Option<Vec<u8>>,
    ) -> (i32, Value) {
        if let Some(context) = context {
            return Self::native_adapter(f, context, command, id, input).await;
        }
        let mut args = vec!["--json".into(), "--request-id".into(), id.to_string()];
        args.extend_from_slice(command);
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let mut child = f.command(&refs);
        child.env("PATH", "");
        if input.is_some() {
            child.stdin(Stdio::piped());
        }
        let mut child = child.spawn().unwrap();
        if let Some(input) = input {
            use tokio::io::AsyncWriteExt;
            let mut stream = child.stdin.take().unwrap();
            tokio::spawn(async move {
                stream.write_all(&input).await.unwrap();
            });
        }
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(15), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            output.status.code().unwrap(),
            serde_json::from_slice(&output.stdout).unwrap(),
        )
    }
    async fn native_adapter(
        f: &Fixture,
        context: Uuid,
        command: &[String],
        id: Uuid,
        input: Option<Vec<u8>>,
    ) -> (i32, Value) {
        use agentchat_protocol::{
            envelope::{Envelope, Outcome},
            operations::{Backend, Target},
        };
        use clap::Parser;
        let mut argv = vec!["agentchat".to_owned()];
        // Exercise current actor grammar, then inject only this harness's private
        // Context at the shared boundary. Real native selection is separately
        // verified using managed hosts; SyntheticHost grants no public authority.
        if !matches!(
            command.first().map(String::as_str),
            Some("install" | "doctor")
        ) {
            argv.extend([
                "--by-host".into(),
                "codex".into(),
                "--by-agent-id".into(),
                "test-owned-native-context".into(),
            ]);
        }
        argv.extend_from_slice(command);
        let parsed = crate::args::Cli::try_parse_from(argv)
            .map_err(|e| agentchat_core::Error::new(ErrorCode::InvalidArgument, e.to_string()));
        let prepared = async {
            let mut cli = parsed?;
            cli.actor.validate(&cli.command)?;
            if let Some(bytes) = input {
                let body = match &mut cli.command {
                    crate::args::Command::Post { body, .. }
                    | crate::args::Command::Reply { body, .. } => body,
                    _ => panic!("stdin is only a message fixture"),
                };
                let text = crate::input::read(
                    crate::args::Body {
                        body: None,
                        stdin: true,
                    },
                    bytes.as_slice(),
                    false,
                )
                .await?;
                body.body = Some(text);
                body.stdin = false;
            }
            let path = cli.project.unwrap_or_else(|| f.workspace.clone());
            let crate::adapt::Action::Finite(operation) = crate::adapt::action(cli.command).await?
            else {
                panic!("finite parity operation")
            };
            Ok::<_, agentchat_core::Error>((path, operation))
        }
        .await;
        let envelope = match prepared {
            Err(error) => Envelope::new(id, None, Err(error)),
            Ok((path, operation)) => {
                let executable = std::path::PathBuf::from(env!("CARGO_BIN_EXE_agentchat"))
                    .canonicalize()
                    .unwrap();
                let setup = agentchat_hosts::setup::ToolSetup::new(
                    f.manager.home().to_owned(),
                    executable,
                    agentchat_hosts::setup::Environment {
                        home: None,
                        user_config_root: Some(f.config.clone()),
                        codex_home: None,
                        claude_config_dir: None,
                    },
                    Some(std::ffi::OsString::new()),
                );
                struct NoLaunch;
                #[async_trait::async_trait]
                impl agentchat_runtime::operations::Launcher for NoLaunch {
                    async fn launch(
                        &self,
                        _: &agentchat_runtime::project::Project,
                        _: &std::path::Path,
                    ) -> agentchat_core::Result<()> {
                        panic!("parity runtime is explicitly owned")
                    }
                }
                agentchat_runtime::operations::Operations::new(
                    f.manager.clone(),
                    Arc::new(NoLaunch),
                )
                .with_host_tools(setup)
                .invoke(
                    id,
                    Target {
                        project_path: Some(path),
                        context_id: Some(context),
                        ..Default::default()
                    },
                    operation,
                )
                .await
            }
        };
        let code = match &envelope.outcome {
            Outcome::Success { .. } => 0,
            Outcome::Failure { error } => error.code.exit_code(),
        };
        (code, serde_json::to_value(envelope).unwrap())
    }
    pub async fn pair(
        &mut self,
        actor: Option<usize>,
        tool: &str,
        args: Value,
        cli: &[&str],
    ) -> Value {
        self.pair_id(actor, tool, args, cli, Uuid::new_v4(), None)
            .await
    }
    pub async fn pair_id(
        &mut self,
        actor: Option<usize>,
        tool: &str,
        args: Value,
        cli: &[&str],
        id: Uuid,
        input: Option<Vec<u8>>,
    ) -> Value {
        let (left, code, right) = self.outputs(actor, tool, args, cli, id, input).await;
        assert_eq!(left, right, "{tool}");
        assert_eq!(code, 0, "{tool}: {right}");
        assert!(left.get("result").is_some(), "{tool}: {left}");
        self.covered.insert(tool.into());
        if tool == "agentchat_join" {
            self.bound[actor.unwrap()] = true;
        }
        left
    }
    pub async fn outputs(
        &mut self,
        actor: Option<usize>,
        tool: &str,
        mut args: Value,
        cli: &[&str],
        id: Uuid,
        input: Option<Vec<u8>>,
    ) -> (Value, i32, Value) {
        let peer = actor.unwrap_or(0);
        args["request_id"] = json!(id);
        if args.get("project_path").is_none() {
            args["project_path"] = json!(self.f.workspace);
        }
        if let Some(actor) = actor
            && (!self.bound[actor] || tool == "agentchat_join")
        {
            args["context_id"] = json!(self.contexts[actor]);
        }
        let command = cli.iter().map(|s| (*s).into()).collect::<Vec<String>>();
        let context = actor.map(|i| self.contexts[i]);
        // Alternate the first transport. Every mutation must be a real first
        // execution on one side and a successful-key replay on the other.
        let (left, (code, right)) = if self.covered.len().is_multiple_of(2) {
            let left = self.peers[peer].call(tool, args).await;
            (
                left,
                Self::cli_raw(&self.f, context, &command, id, input).await,
            )
        } else {
            let right = Self::cli_raw(&self.f, context, &command, id, input).await;
            (self.peers[peer].call(tool, args).await, right)
        };
        (left, code, right)
    }
    pub async fn error(
        &mut self,
        actor: usize,
        tool: &str,
        args: Value,
        cli: &[&str],
        expected: ErrorCode,
    ) -> Value {
        let id = args
            .get("request_id")
            .map(|id| Uuid::parse_str(id.as_str().unwrap()).unwrap())
            .unwrap_or_else(Uuid::new_v4);
        let (left, code, right) = self.outputs(Some(actor), tool, args, cli, id, None).await;
        assert_eq!(left, right, "{tool}");
        assert_eq!(code, expected.exit_code(), "{tool}: {right}");
        assert_eq!(
            left["error"]["code"],
            serde_json::to_value(expected).unwrap(),
            "{tool}: {left}"
        );
        self.errors.insert(tool.into());
        left
    }
    pub async fn seed(
        &self,
        actor: usize,
        commands: impl IntoIterator<Item = Mutation>,
    ) -> Vec<Value> {
        let mut client = Client::connect(&self.runtime.paths().socket, self.project_id)
            .await
            .unwrap();
        let mut values = Vec::new();
        for command in commands {
            let request_id = Uuid::new_v4();
            values.push(
                success(
                    client
                        .call(
                            request_id,
                            Call::Mutate {
                                request_id,
                                actor: agentchat_protocol::rpc::ActorReference::Native {
                                    context_id: self.contexts[actor],
                                },
                                command,
                            },
                        )
                        .await
                        .unwrap(),
                )
                .unwrap(),
            );
        }
        values
    }
    pub async fn names(&mut self) -> BTreeSet<String> {
        let tools = self.peers[0].request("tools/list", json!({})).await;
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    }
    pub async fn finish(mut self) {
        let names = self.names().await;
        assert_eq!(
            self.covered, names,
            "every advertised tool must execute a successful paired case"
        );
        self.close().await;
    }
    pub async fn close(mut self) {
        for peer in self.peers.drain(..) {
            peer.close().await;
        }
        self.runtime.shutdown();
        self.task.take().unwrap().await.unwrap().unwrap();
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.runtime.shutdown();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
