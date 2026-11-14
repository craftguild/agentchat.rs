//! Real Unix-socket fixture with explicit synthetic-only liveness evidence.

#![allow(dead_code)]

use super::{Fixture, SyntheticHost};
use agentchat_core::{
    Result,
    application::Mutation,
    model::{Context, HostKind},
    read::Query,
    time::{Clock, Timestamp},
};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::{
    context::{ContextRegistry, ContextSeed},
    daemon::{RuntimeHandle, RuntimeOptions, Server},
    project::Project,
};
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub struct ManualClock(Mutex<Timestamp>);
impl Default for ManualClock {
    fn default() -> Self {
        Self(Mutex::new(
            Timestamp::parse("2026-10-15T00:00:00.000Z").unwrap(),
        ))
    }
}
impl ManualClock {
    pub fn advance(&self, seconds: i64) {
        let mut time = self.0.lock().unwrap();
        *time = time.after_seconds(seconds).unwrap();
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap()
    }
}
pub fn options() -> RuntimeOptions {
    RuntimeOptions {
        idle: Duration::from_secs(10),
        io_timeout: Duration::from_secs(1),
        housekeeping: Duration::from_millis(20),
    }
}
pub fn seed(f: &Fixture, session: &str) -> ContextSeed {
    ContextSeed {
        host: HostKind::Codex,
        host_session_id: session.into(),
        host_version: "fixture-only".into(),
        os_profile: "synthetic-not-admitted".into(),
        working_directory: f.workspace.clone(),
    }
}
pub struct Running {
    pub f: Fixture,
    pub project: Project,
    pub clock: Arc<ManualClock>,
    pub handle: RuntimeHandle,
    pub pipeline: agentchat_runtime::daemon::NativePipeline,
    host: Arc<dyn agentchat_core::ports::HostGate>,
    task: Option<JoinHandle<Result<()>>>,
}
impl Running {
    pub async fn new(options: RuntimeOptions) -> Self {
        Self::with_delivery(options, None).await
    }
    pub async fn with_delivery(
        options: RuntimeOptions,
        delivery: Option<Arc<dyn agentchat_core::delivery::DeliveryPort>>,
    ) -> Self {
        Self::with_host(options, delivery, Arc::new(SyntheticHost)).await
    }
    pub async fn with_host(
        options: RuntimeOptions,
        delivery: Option<Arc<dyn agentchat_core::delivery::DeliveryPort>>,
        host: Arc<dyn agentchat_core::ports::HostGate>,
    ) -> Self {
        Self::with_ports(options, delivery, None, false, host).await
    }
    pub async fn with_message_host(
        options: RuntimeOptions,
        messages: Arc<dyn agentchat_core::delivery::MessagePort>,
        host: Arc<dyn agentchat_core::ports::HostGate>,
    ) -> Self {
        Self::with_ports(options, None, Some(messages), false, host).await
    }
    pub async fn with_native_host(
        options: RuntimeOptions,
        messages: Arc<dyn agentchat_core::delivery::MessagePort>,
        host: Arc<dyn agentchat_core::ports::HostGate>,
    ) -> Self {
        Self::with_ports(options, None, Some(messages), true, host).await
    }
    async fn with_ports(
        options: RuntimeOptions,
        delivery: Option<Arc<dyn agentchat_core::delivery::DeliveryPort>>,
        messages: Option<Arc<dyn agentchat_core::delivery::MessagePort>>,
        native: bool,
        host: Arc<dyn agentchat_core::ports::HostGate>,
    ) -> Self {
        let f = Fixture::new();
        let project = f
            .manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap()
            .project;
        let clock = Arc::new(ManualClock::default());
        let server = Server::bind(
            f.manager.clone(),
            project.clone(),
            host.clone(),
            clock.clone(),
            options,
        )
        .await
        .unwrap();
        let server = if let Some(delivery) = delivery {
            server.with_delivery(delivery)
        } else {
            server
        };
        let server = if let Some(messages) = messages {
            if native {
                server.with_native_messages(messages)
            } else {
                server.with_messages(messages)
            }
        } else {
            server
        };
        let pipeline = server.native_pipeline();
        let handle = server.handle();
        let task = Some(tokio::spawn(server.run()));
        Self {
            f,
            project,
            clock,
            handle,
            pipeline,
            host,
            task,
        }
    }
    pub async fn application(&self) -> agentchat_core::application::Application {
        agentchat_core::application::Application::new(
            self.f.manager.open(&self.project).await.unwrap(),
            self.clock.clone(),
            self.host.clone(),
        )
    }
    /// Explicit synthetic full-input receipt after observing each exact claim.
    /// Hint presentation and public reads themselves never settle this stream.
    pub async fn accept_native(&self, context: &Context, ids: Vec<Uuid>) {
        accept_native(&self.application().await, context, ids).await;
    }
    pub async fn client(&self) -> Client {
        Client::connect(&self.handle.paths().socket, self.project.identity().id)
            .await
            .unwrap()
    }
    pub async fn context(&self, session: &str) -> Context {
        ContextRegistry::new(self.f.manager.clone())
            .issue(seed(&self.f, session))
            .await
            .unwrap()
    }
    pub fn finished(&self) -> bool {
        self.task.as_ref().unwrap().is_finished()
    }
    pub async fn stop(mut self) {
        self.handle.shutdown();
        tokio::time::timeout(Duration::from_secs(5), self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!self.handle.paths().socket.exists());
    }
    pub async fn restart(&mut self, options: RuntimeOptions) {
        self.restart_with_delivery(options, None).await;
    }
    pub async fn restart_with_delivery(
        &mut self,
        options: RuntimeOptions,
        delivery: Option<Arc<dyn agentchat_core::delivery::DeliveryPort>>,
    ) {
        self.handle.shutdown();
        tokio::time::timeout(Duration::from_secs(5), self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let server = Server::bind(
            self.f.manager.clone(),
            self.project.clone(),
            Arc::new(SyntheticHost),
            self.clock.clone(),
            options,
        )
        .await
        .unwrap();
        let server = if let Some(delivery) = delivery {
            server.with_delivery(delivery)
        } else {
            server
        };
        self.pipeline = server.native_pipeline();
        self.handle = server.handle();
        self.task = Some(tokio::spawn(server.run()));
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.handle.shutdown();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
pub async fn query(client: &mut Client, context: Option<Uuid>, query: Query) -> Value {
    serde_json::to_value(
        success(client.query(Uuid::new_v4(), context, query).await.unwrap()).unwrap(),
    )
    .unwrap()
}
pub async fn whoami(client: &mut Client) -> Value {
    query(client, None, Query::Whoami).await
}
pub async fn health(client: &mut Client) -> Value {
    serde_json::to_value(client.health().await.unwrap()).unwrap()
}
pub async fn assert_automatic_receipts(client: &mut Client, message: Uuid, recipients: usize) {
    let detail = query(
        client,
        None,
        Query::Message {
            message_id: message,
            receipts: Default::default(),
        },
    )
    .await;
    let rows = detail["receipts"]["items"].as_array().unwrap();
    assert_eq!(rows.len(), recipients);
    for row in rows {
        let delivery = &row["delivery"];
        assert!(!delivery["delivered_at"].is_null());
        assert!(delivery.get("acknowledged_at").is_none());
        let reads = row["reads"].as_array().unwrap();
        assert_eq!(reads.len(), 1, "no explicit read is implied by acceptance");
        assert_eq!(reads[0]["kind"], "automatic");
        assert_eq!(reads[0]["at"], delivery["delivered_at"]);
    }
}
pub async fn set_confirmation_timeout(r: &Running, seconds: u32) {
    let store = r.f.manager.open(&r.project).await.unwrap();
    agentchat_core::application::configure_project_with_settings(
        store.as_ref(),
        r.clock.as_ref(),
        None,
        agentchat_core::project::ReceiptSettingsUpdate {
            delivery_confirmation_timeout_seconds: Some(seconds),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}
pub async fn mutate(client: &mut Client, context: &Context, command: Mutation) -> Value {
    success(
        client
            .call(
                Uuid::new_v4(),
                Call::Mutate {
                    request_id: Uuid::new_v4(),
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: context.context_id,
                    },
                    command,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap()
}
pub async fn join(client: &mut Client, context: &Context, name: &str) -> Value {
    mutate(
        client,
        context,
        Mutation::Join {
            name: Some(name.into()),
        },
    )
    .await
}
pub async fn attach(client: &mut Client, context: &Context) -> Value {
    success(
        client
            .call(
                Uuid::new_v4(),
                Call::Attach {
                    context_id: context.context_id,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap()
}

/// Test consumers can use either the singleton Core API or the production queue.
#[async_trait::async_trait]
pub trait NativeInputService: Send + Sync {
    async fn prepare(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::delivery::NativeDispatch>>;
    async fn finish(
        &self,
        context: &Context,
        dispatch: &agentchat_core::delivery::NativeDispatch,
        outcome: Result<agentchat_core::delivery::MessageProgress>,
    ) -> Result<bool>;
}
#[async_trait::async_trait]
impl<T: NativeInputService + ?Sized> NativeInputService for Arc<T> {
    async fn prepare(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::delivery::NativeDispatch>> {
        self.as_ref().prepare(context).await
    }
    async fn finish(
        &self,
        context: &Context,
        dispatch: &agentchat_core::delivery::NativeDispatch,
        outcome: Result<agentchat_core::delivery::MessageProgress>,
    ) -> Result<bool> {
        self.as_ref().finish(context, dispatch, outcome).await
    }
}
#[async_trait::async_trait]
impl NativeInputService for agentchat_core::application::Application {
    async fn prepare(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::delivery::NativeDispatch>> {
        self.prepare_native(context).await
    }
    async fn finish(
        &self,
        context: &Context,
        dispatch: &agentchat_core::delivery::NativeDispatch,
        outcome: Result<agentchat_core::delivery::MessageProgress>,
    ) -> Result<bool> {
        self.finish_native(context, dispatch, outcome).await
    }
}
#[async_trait::async_trait]
impl NativeInputService for agentchat_runtime::daemon::NativePipeline {
    async fn prepare(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::delivery::NativeDispatch>> {
        self.prepare_native(context).await
    }
    async fn finish(
        &self,
        context: &Context,
        dispatch: &agentchat_core::delivery::NativeDispatch,
        outcome: Result<agentchat_core::delivery::MessageProgress>,
    ) -> Result<bool> {
        self.finish_native(context, dispatch, outcome).await
    }
}

/// Test consumer receives and confirms complete inputs through the normal API.
pub async fn accept_native(
    app: &(impl NativeInputService + ?Sized),
    context: &Context,
    ids: Vec<Uuid>,
) {
    accept_native_observed(app, context, ids, |_| {}).await;
}

/// Test-only observations do not grant a receipt or bypass the ordinary claim.
pub enum NativeObservation<'a> {
    Preparing,
    Prepared(&'a agentchat_core::delivery::NativeDispatch),
    Committed(&'a agentchat_core::delivery::NativeDispatch),
}

pub async fn accept_native_observed(
    app: &(impl NativeInputService + ?Sized),
    context: &Context,
    ids: Vec<Uuid>,
    mut observe: impl FnMut(NativeObservation<'_>) + Send,
) {
    let mut remaining: std::collections::HashSet<_> = ids.into_iter().collect();
    for _ in 0..1_000 {
        if remaining.is_empty() {
            return;
        }
        observe(NativeObservation::Preparing);
        let dispatch = app
            .prepare(context)
            .await
            .unwrap()
            .expect("eligible complete input");
        assert!(
            !dispatch.awaiting_acceptance(),
            "this consumer cannot invent an original recovery result"
        );
        observe(NativeObservation::Prepared(&dispatch));
        let received = dispatch.input();
        assert_eq!(
            agentchat_core::delivery::NativeInput::from_text(&received.text().unwrap()).unwrap(),
            received
        );
        remaining.remove(&dispatch.delivery_id());
        assert!(
            app.finish(
                context,
                &dispatch,
                Ok(agentchat_core::delivery::MessageProgress::Accepted(
                    agentchat_core::delivery::HostReceipt::default()
                ))
            )
            .await
            .unwrap()
        );
        // Only this successful FULL return may advance a durable counter.
        observe(NativeObservation::Committed(&dispatch));
    }
    panic!("bounded synthetic input consumer did not reach expected deliveries");
}
