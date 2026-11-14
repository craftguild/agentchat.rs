//! Actual Core/Store empty-preparation barrier with private synthetic ownership.
use crate::daemon::delivery::{DispatchPort, Dispatches, input};
use crate::diagnostics::RuntimeLog;
use crate::{
    context::ContextRegistry,
    daemon::{activity::Activity, coordinator::Worker},
    project::{InitRequest, Project, ProjectManager},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    application::{Application, Mutation},
    delivery::{
        HostReceipt, MessageDispatch, MessagePort, MessageProgress, NativeDispatch, NoticeDispatch,
    },
    model::{Context, Delivery, HostKind, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState, Storage},
    project::ReceiptSettingsUpdate,
    sequence::Cursor,
    time::{Clock, Timestamp},
};
use agentchat_store::Store;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::watch;
use tokio::sync::{Semaphore, mpsc};
use uuid::Uuid;
struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::parse("2026-10-15T00:00:00.000Z").unwrap()
    }
}
struct Host(Mutex<HashMap<Uuid, LaunchIdentity>>);
#[async_trait::async_trait]
impl HostGate for Host {
    fn input_capabilities(
        &self,
        _: &Context,
    ) -> Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::RetainUncertainty,
            ActiveInput::ExactTurnId,
        ))
    }
    async fn launch_identity(&self, c: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self.0.lock().unwrap().get(&c.context_id).cloned())
    }
    fn launch_owner(&self, i: &LaunchIdentity) -> LaunchOwnerState {
        if self.0.lock().unwrap().values().any(|v| v == i) {
            LaunchOwnerState::Current
        } else {
            LaunchOwnerState::Unknown
        }
    }
    async fn verify(&self, _: &Context) -> Result<HostState> {
        Ok(HostState {
            phase: Phase::Idle,
            active_turn_id: None,
            wake_paused: false,
        })
    }
}
#[derive(Default)]
pub(super) struct Port {
    pub fresh: AtomicUsize,
    pub polls: AtomicUsize,
    pub uncertain: AtomicBool,
}
impl Port {
    fn result(&self, awaiting: bool) -> Result<MessageProgress> {
        if awaiting {
            self.polls.fetch_add(1, Ordering::SeqCst);
        } else {
            self.fresh.fetch_add(1, Ordering::SeqCst);
        }
        if self.uncertain.load(Ordering::SeqCst) {
            Err(Error::new(
                ErrorCode::RuntimeUnavailable,
                "synthetic original native result unavailable",
            ))
        } else {
            Ok(MessageProgress::Accepted(HostReceipt::default()))
        }
    }
}
#[async_trait::async_trait]
impl MessagePort for Port {
    async fn present_message(&self, _: &Context, d: &MessageDispatch) -> Result<MessageProgress> {
        self.result(d.awaiting_acceptance)
    }
    async fn present_notice(&self, _: &Context, d: &NoticeDispatch) -> Result<MessageProgress> {
        self.result(d.awaiting_acceptance)
    }
}
pub(super) struct Gate {
    pub armed: AtomicUsize,
    pub calls: AtomicUsize,
    pub entered: mpsc::UnboundedSender<Uuid>,
    pub resume: Semaphore,
}
struct Preparation {
    app: Arc<Application>,
    gate: Arc<Gate>,
}
#[async_trait::async_trait]
impl input::Preparation for Preparation {
    async fn prepare(&self, c: &Context) -> Result<Option<NativeDispatch>> {
        self.gate.calls.fetch_add(1, Ordering::SeqCst);
        let result = self.app.prepare_native(c).await?;
        if result.is_none()
            && self
                .gate
                .armed
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
        {
            self.gate.entered.send(c.context_id).unwrap();
            self.gate.resume.acquire().await.unwrap().forget();
        }
        Ok(result)
    }
}
pub(super) struct Fixture {
    pub _root: TempDir,
    pub app: Arc<Application>,
    pub store: Arc<Store>,
    pub contexts: Vec<Context>,
    pub port: Arc<Port>,
    pub gate: Arc<Gate>,
    pub entered: mpsc::UnboundedReceiver<Uuid>,
    pub log: RuntimeLog,
    manager: ProjectManager,
    project: Project,
}
impl Fixture {
    pub async fn new(n: usize, hold: usize) -> Self {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let manager = ProjectManager::new(root.path().join("state")).unwrap();
        let project = manager
            .initialize(&workspace, InitRequest::new(Uuid::new_v4()))
            .await
            .unwrap()
            .project;
        let identity = project.identity().clone();
        let store = manager.open(&project).await.unwrap();
        let host = Arc::new(Host(Mutex::new(HashMap::new())));
        let app = Arc::new(Application::new(
            store.clone(),
            Arc::new(FixedClock),
            host.clone(),
        ));
        app.initialize_with_settings(
            None,
            ReceiptSettingsUpdate {
                auto_read: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut contexts = Vec::new();
        for i in 0..n {
            let c = Context {
                context_id: Uuid::new_v4(),
                project_id: identity.id,
                host: HostKind::Codex,
                host_session_id: format!("rearm-{i}"),
                host_version: "synthetic-rearm-only".into(),
                os_profile: "synthetic-not-admitted".into(),
                working_directory: workspace.clone(),
                incarnation: Uuid::new_v4(),
            };
            host.0.lock().unwrap().insert(
                c.context_id,
                LaunchIdentity {
                    generation: Uuid::new_v4(),
                    owner: "explicit-test-owned".into(),
                },
            );
            app.start_launch(&c, Some(&format!("receiver{i}")))
                .await
                .unwrap();
            contexts.push(c);
        }
        for c in &contexts {
            while let Some(d) = app.prepare_native(c).await.unwrap() {
                assert!(
                    app.finish_native(c, &d, Ok(MessageProgress::Accepted(Default::default())))
                        .await
                        .unwrap()
                );
            }
        }
        let (entered, receiver) = mpsc::unbounded_channel();
        let gate = Arc::new(Gate {
            armed: AtomicUsize::new(hold),
            calls: AtomicUsize::new(0),
            entered,
            resume: Semaphore::new(0),
        });
        let port = Arc::new(Port::default());
        use std::os::unix::fs::DirBuilderExt;
        let data = root.path().join("private-runtime-data");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&data)
            .unwrap();
        let log = RuntimeLog::open(&data, identity.id, Uuid::new_v4())
            .await
            .unwrap();
        Self {
            _root: root,
            app,
            store,
            contexts,
            port,
            gate,
            entered: receiver,
            log,
            manager,
            project,
        }
    }
    pub async fn worker(&self) -> Worker {
        let (heads, _) = watch::channel(self.head().await);
        Worker {
            app: self.app.clone(),
            manager: self.manager.clone(),
            project: self.project.clone(),
            registry: ContextRegistry::new(self.manager.clone()),
            heads,
            activity: Activity::new(),
            attached: self
                .contexts
                .iter()
                .map(|c| (Uuid::new_v4(), c.clone()))
                .collect(),
            pending: HashMap::new(),
            last_error: None,
            instance: Uuid::new_v4(),
            deliveries: self.dispatches(),
            staging: agentchat_hosts::staging::OriginReaper::new(self.manager.home())
                .with_context_validator(ContextRegistry::validate_stage),
            diagnostics: self.log.sink(),
            commits: None,
            grouping: Default::default(),
            native_jobs_completed: false,
            native_empty_at: None,
        }
    }
    pub fn dispatches(&self) -> Dispatches {
        Dispatches::new(
            Some(DispatchPort::Native(self.port.clone())),
            self.log.sink(),
        )
        .with_preparation(Arc::new(Preparation {
            app: self.app.clone(),
            gate: self.gate.clone(),
        }))
    }
    pub async fn barrier(&mut self, n: usize) {
        for _ in 0..n {
            tokio::time::timeout(Duration::from_secs(3), self.entered.recv())
                .await
                .unwrap()
                .unwrap();
        }
    }
    pub async fn post(&self) -> Uuid {
        let r = self
            .app
            .execute(
                Uuid::new_v4(),
                &self.contexts[0],
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "@receiver1 original later durable work".into(),
                },
            )
            .await
            .unwrap();
        serde_json::from_value(r.result["message_id"].clone()).unwrap()
    }
    pub async fn head(&self) -> Cursor {
        self.app.event_head().await.unwrap()
    }
    pub async fn stream(&self, i: usize) -> agentchat_core::delivery::MessageStream {
        let mut tx = self.store.begin().await.unwrap();
        let c = &self.contexts[i];
        let a = tx
            .conversation(c.host, &c.host_session_id)
            .await
            .unwrap()
            .unwrap();
        let g = tx
            .latest_launch(a.id)
            .await
            .unwrap()
            .unwrap()
            .identity
            .generation;
        let stream = tx.message_stream(g).await.unwrap().unwrap();
        tx.rollback().await.unwrap();
        stream
    }
    pub async fn next(&self, i: usize) -> Option<Delivery> {
        let mut tx = self.store.begin().await.unwrap();
        let c = &self.contexts[i];
        let a = tx
            .conversation(c.host, &c.host_session_id)
            .await
            .unwrap()
            .unwrap();
        let g = tx
            .latest_launch(a.id)
            .await
            .unwrap()
            .unwrap()
            .identity
            .generation;
        let stream = tx.message_stream(g).await.unwrap().unwrap();
        let d = tx
            .next_native(a.id, stream.settled_seq)
            .await
            .unwrap()
            .map(|d| d.delivery().clone());
        tx.rollback().await.unwrap();
        d
    }
}
