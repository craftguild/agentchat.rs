//! Actual executable/runtime ownership for the explicit browser integration harness.
use crate::support::Fixture;
use agentchat_core::{Result, application::Mutation, model::Context, read::requests};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::{
    daemon::{RuntimeHandle, RuntimePaths},
    project::Project,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};
use uuid::Uuid;

pub struct BrowserFixture {
    pub fixture: Fixture,
    pub project: Project,
    pub actor: Context,
    pub author_id: Uuid,
    pub client: Client,
    pub thread_root: Uuid,
    pub old_root: Uuid,
    pub roots: Vec<Uuid>,
    pub participants: Vec<Context>,
    pub created_participants: u64,
    handle: RuntimeHandle,
    task: Option<JoinHandle<Result<()>>>,
    viewer: Option<Child>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    producer: Option<JoinHandle<()>>,
    executable: PathBuf,
    preflight_seconds: f64,
    image: String,
}
impl BrowserFixture {
    pub async fn new() -> Self {
        let fixture = Fixture::new();
        // Record ordinary offline startup before project/actor mutation. The
        // same frozen copy is then used by the viewer with its existing ready
        // deadline. This neither changes that deadline nor host controls.
        let source = std::env::var_os("AGENTCHAT_REAL_BROWSER_CLI_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_agentchat").into());
        let executable = fixture.root.path().join("agentchat-browser-proof");
        std::fs::copy(source, &executable).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o500)).unwrap();
        let image = Sha256::digest(std::fs::read(&executable).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let started = tokio::time::Instant::now();
        let offline = tokio::time::timeout(
            Duration::from_secs(300),
            Command::new(&executable)
                .arg("--version")
                .current_dir(&fixture.workspace)
                .env("AGENTCHAT_HOME", fixture.manager.home())
                .env("AGENTCHAT_USER_CONFIG_ROOT", &fixture.config)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("owned offline CLI preflight deadline")
        .unwrap();
        let preflight_seconds = started.elapsed().as_secs_f64();
        assert!(offline.status.success());
        assert!(offline.stderr.is_empty() && !offline.stdout.is_empty());
        println!(
            "AGENTCHAT_BROWSER_CLI_PREFLIGHT={}",
            json!({"seconds":preflight_seconds,"sha256":image,"exit_code":0,
                "scope":"ordinary offline version before fixture mutation; no viewer timing claim"})
        );
        let project = fixture.initialize().await;
        let (handle, task) = fixture.server(&project).await;
        let actor = fixture.context("real-browser-synthetic-author").await;
        let client = Client::connect(
            RuntimePaths::for_project(&project).socket,
            project.identity().id,
        )
        .await
        .unwrap();
        Self {
            fixture,
            project,
            actor,
            author_id: Uuid::nil(),
            client,
            thread_root: Uuid::nil(),
            old_root: Uuid::nil(),
            roots: Vec::new(),
            participants: Vec::new(),
            created_participants: 0,
            handle,
            task: Some(task),
            viewer: None,
            stderr: None,
            producer: None,
            executable,
            preflight_seconds,
            image,
        }
    }
    pub async fn start_viewer(&mut self) -> String {
        self.start_producer().await;
        // A cold executable may start after the ordinary runtime idle deadline.
        // Hold only test-owned observer activity until the actual viewer takes
        // over; preserve the original instance and all product lifecycle timers.
        let observer = Client::connect(
            RuntimePaths::for_project(&self.project).socket,
            self.project.identity().id,
        )
        .await
        .unwrap();
        assert_eq!(observer.instance_id(), self.client.instance_id());
        let startup = observer.observe().await.unwrap();
        // The frozen, preflighted image receives only disposable roots and no
        // usable agent context; its actual viewer-ready deadline is unchanged.
        let mut child = Command::new(&self.executable)
            .args(["viewer", "--json"])
            .current_dir(&self.fixture.workspace)
            .env("AGENTCHAT_HOME", self.fixture.manager.home())
            .env("AGENTCHAT_USER_CONFIG_ROOT", &self.fixture.config)
            .env("AGENTCHAT_CONTEXT", Uuid::new_v4().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        self.stderr = Some(tokio::spawn(async move {
            let mut bytes = Vec::new();
            stderr.take(32_769).read_to_end(&mut bytes).await.unwrap();
            bytes
        }));
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        self.viewer = Some(child);
        let line = tokio::time::timeout(Duration::from_secs(180), output.next_line())
            .await
            .expect("owned viewer ready deadline")
            .unwrap()
            .unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["version"], 1);
        assert_eq!(ready["project_id"], self.project.identity().id.to_string());
        assert!(!self.task.as_ref().unwrap().is_finished());
        drop(startup);
        ready["result"]["url"].as_str().unwrap().to_owned()
    }

    async fn start_producer(&mut self) {
        assert!(self.producer.is_none());
        let mut client = Client::connect(
            RuntimePaths::for_project(&self.project).socket,
            self.project.identity().id,
        )
        .await
        .unwrap();
        assert_eq!(client.instance_id(), self.client.instance_id());
        let context_id = self.actor.context_id;
        // This exact synthetic producer, not the context-free Viewer, owns
        // liveness. Keep the ordinary Core heartbeat/expiry checks even when a
        // cold copied CLI has not entered main yet. No Attach/Join or read is
        // manufactured by this fixture-only timer.
        success(
            client
                .call(Uuid::new_v4(), Call::Heartbeat { context_id })
                .await
                .unwrap(),
        )
        .unwrap();
        self.producer = Some(tokio::spawn(async move {
            let period = Duration::from_secs(15);
            let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                success(
                    client
                        .call(Uuid::new_v4(), Call::Heartbeat { context_id })
                        .await
                        .unwrap(),
                )
                .unwrap();
            }
        }));
    }

    async fn stop_producer(&mut self) {
        if let Some(task) = self.producer.take() {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
    }
    pub async fn status(&mut self) -> Value {
        // Mutations bind the control connection to `actor`; viewer-only reads
        // must retain the same context-free observer boundary as the product.
        let mut observer = Client::connect(
            RuntimePaths::for_project(&self.project).socket,
            self.project.identity().id,
        )
        .await
        .unwrap();
        let head = success(
            observer
                .read(Uuid::new_v4(), None, requests::ViewerHead)
                .await
                .unwrap(),
        )
        .unwrap();
        let status = self.client.health().await.unwrap();
        assert_eq!(
            status.application.statistics.agents,
            1 + self.created_participants
        );
        assert_eq!(
            status.application.statistics.joined,
            1 + self.participants.len() as u64
        );
        assert_eq!(status.application.statistics.read_receipts, 0);
        assert!(
            serde_json::to_value(&status.application.statistics)
                .unwrap()
                .get("acknowledged_deliveries")
                .is_none()
        );
        assert_eq!(status.runtime.activity.attached_connections, 0);
        assert_eq!(status.runtime.native_delivery.jobs, 0);
        json!({"head":head,"statistics":status.application.statistics,
            "runtime_instance":self.client.instance_id(),
            "author_id":self.author_id,"author_context":self.actor.context_id,
            "observers":status.runtime.activity.observers,"attached_connections":0,
            "thread_root":self.thread_root,"old_root":self.old_root,"roots":self.roots,
            "cli_image_sha256":self.image,"cli_offline_preflight_seconds":self.preflight_seconds,
            "viewer_running":self.viewer.is_some()})
    }
    pub async fn restart(&mut self) {
        self.stop_producer().await;
        let original = self.client.instance_id();
        self.handle.shutdown();
        self.task.take().unwrap().await.unwrap().unwrap();
        let (handle, task) = self.fixture.server(&self.project).await;
        self.handle = handle;
        self.task = Some(task);
        self.client = Client::connect(
            RuntimePaths::for_project(&self.project).socket,
            self.project.identity().id,
        )
        .await
        .unwrap();
        assert_ne!(self.client.instance_id(), original);
        // Shutdown invalidates producer liveness. Refresh only this fixture's
        // exact synthetic author through ordinary Join; the viewer never gets
        // its descriptor, participates or bypasses the liveness guard.
        let session = self.actor.host_session_id.clone();
        self.actor = self.fixture.context(&session).await;
        let request_id = Uuid::new_v4();
        let joined = success(
            self.client
                .call(
                    request_id,
                    Call::Mutate {
                        request_id,
                        actor: agentchat_protocol::rpc::ActorReference::Native {
                            context_id: self.actor.context_id,
                        },
                        command: Mutation::Join {
                            name: Some("author".into()),
                        },
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(joined["agent_id"], self.author_id.to_string());
        self.start_producer().await;
    }
    pub async fn stop_viewer(&mut self) {
        if let Some(mut child) = self.viewer.take() {
            let pid = child.id().unwrap().to_string();
            assert!(
                Command::new("kill")
                    .args(["-INT", &pid])
                    .status()
                    .await
                    .unwrap()
                    .success()
            );
            assert!(
                tokio::time::timeout(Duration::from_secs(6), child.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .success()
            );
            let stderr = self.stderr.take().unwrap().await.unwrap();
            assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if self
                        .client
                        .health()
                        .await
                        .unwrap()
                        .runtime
                        .activity
                        .observers
                        == 0
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(!self.task.as_ref().unwrap().is_finished());
        }
    }
    pub async fn close(&mut self) {
        self.stop_viewer().await;
        self.stop_producer().await;
        self.handle.shutdown();
        self.task.take().unwrap().await.unwrap().unwrap();
    }
}
impl Drop for BrowserFixture {
    fn drop(&mut self) {
        if let Some(task) = &self.producer {
            task.abort();
        }
        if let Some(child) = &mut self.viewer {
            let _ = child.start_kill();
        }
        self.handle.shutdown();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
