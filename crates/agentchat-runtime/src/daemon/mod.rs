//! Owned local runtime: bounded RPC, application coordination and observer follow.

mod activity;
mod change_follow;
mod connection;
mod coordinator;
mod delivery;
mod follow;
mod metrics;
mod native_pipeline;
pub use native_pipeline::NativePipeline;
mod ownership;
mod requests;
pub use metrics::CommitObserver;
pub use ownership::RuntimePaths;

use crate::{
    diagnostics::{Event, RuntimeLog, Sink},
    project::{Project, ProjectManager},
};
use activity::Activity;
use agentchat_core::{
    Error, ErrorCode, Result, application::Application, ports::HostGate, time::Clock,
};
use ownership::Ownership;
use std::{sync::Arc, time::Duration};
use tokio::{
    net::UnixListener,
    sync::{Semaphore, watch},
    task::JoinSet,
};
use uuid::Uuid;

/// Runtime defaults implement the v1 lifecycle; fixture timers may be shortened.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeOptions {
    /// No valid attachment/observer/request for this duration ends the runtime.
    pub idle: Duration,
    /// Deadline for bounded socket writes and the initial handshake.
    pub io_timeout: Duration,
    /// Lease/presence housekeeping; at most the minimum TTL renewal interval.
    pub housekeeping: Duration,
}
impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(60),
            io_timeout: Duration::from_secs(5),
            housekeeping: Duration::from_secs(1),
        }
    }
}
/// Owner-side lifecycle handle. There is no public RPC shutdown endpoint.
#[derive(Clone)]
pub struct RuntimeHandle {
    shutdown: watch::Sender<bool>,
    paths: RuntimePaths,
    instance: Uuid,
    diagnostics: Sink,
}
impl RuntimeHandle {
    /// Ask this owned instance to close its sockets and disconnect its bindings.
    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }
    /// Checked endpoint paths for clients.
    pub fn paths(&self) -> &RuntimePaths {
        &self.paths
    }
    /// Distinct UUID for every runtime lifetime.
    pub fn instance_id(&self) -> Uuid {
        self.instance
    }
    /// Nonblocking diagnostic health/budgets without exposing log contents.
    pub fn diagnostics(&self) -> agentchat_protocol::health::DiagnosticHealth {
        self.diagnostics.status()
    }
}
/// One exclusively owned runtime. Host adapters are injected independently.
pub struct Server {
    manager: ProjectManager,
    project: Project,
    app: Arc<Application>,
    listener: UnixListener,
    owner: Ownership,
    paths: RuntimePaths,
    options: RuntimeOptions,
    shutdown: watch::Sender<bool>,
    instance: Uuid,
    delivery: Option<delivery::DispatchPort>,
    log: RuntimeLog,
    commits: Option<Arc<dyn CommitObserver>>,
    queue: coordinator::Queue,
}
impl Server {
    /// Claim ownership before opening storage; live owners are never unlinked.
    pub async fn bind(
        manager: ProjectManager,
        project: Project,
        host: Arc<dyn HostGate>,
        clock: Arc<dyn Clock>,
        options: RuntimeOptions,
    ) -> Result<Self> {
        if options.idle.is_zero()
            || options.io_timeout.is_zero()
            || options.housekeeping.is_zero()
            || options.housekeeping > Duration::from_secs(10)
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Runtime timers must be positive and housekeeping at most ten seconds.",
            ));
        }
        let _fence = manager.installation_fence().await?;
        manager.validate(&project).await?;
        let paths = RuntimePaths::for_project(&project);
        let mut owner = Ownership::claim(paths.clone(), &project).await?;
        let store = manager.open(&project).await?;
        let app = Arc::new(Application::new(store, clock, host));
        let listener = owner.listen().await?;
        let (shutdown, _) = watch::channel(false);
        let instance = Uuid::new_v4();
        let log = RuntimeLog::open(project.data_dir(), project.identity().id, instance).await?;
        Ok(Self {
            manager,
            project,
            app,
            listener,
            owner,
            paths,
            options,
            shutdown,
            instance,
            delivery: None,
            log,
            commits: None,
            queue: coordinator::Queue::new(),
        })
    }
    /// Compose a native presentation adapter after claiming runtime ownership.
    /// The host gate must independently admit the exact profile; this builder
    /// never grants participation or manufactures a production capability.
    pub fn with_delivery(
        mut self,
        delivery: Arc<dyn agentchat_core::delivery::DeliveryPort>,
    ) -> Self {
        self.delivery = Some(delivery::DispatchPort::Hint(delivery));
        self
    }
    /// Select the durable full-body stream in the existing bounded job loop.
    /// This replaces legacy hint dispatch for this runtime; compatibility, native
    /// startup and final root/control/claim checks remain independently required.
    pub fn with_messages(mut self, port: Arc<dyn agentchat_core::delivery::MessagePort>) -> Self {
        self.delivery = Some(delivery::DispatchPort::Message(port));
        self
    }
    /// Select one ordered native stream for complete messages and bounded event
    /// context. This is the ordinary daemon composition after adapter selection.
    pub fn with_native_messages(
        mut self,
        port: Arc<dyn agentchat_core::delivery::MessagePort>,
    ) -> Self {
        self.delivery = Some(delivery::DispatchPort::Native(port));
        self
    }
    /// Observe committed application returns without changing transaction,
    /// scheduling, participation or host admission policy.
    pub fn with_commit_observer(mut self, observer: Arc<dyn CommitObserver>) -> Self {
        self.commits = Some(observer);
        self
    }
    /// Share this verified runtime owner's application for in-process composition.
    ///
    /// The returned service retains this project's existing storage writer and
    /// host gates. It accepts no replacement application, database or authority;
    /// native preparations and RPC mutations use the same connection pool.
    pub fn application(&self) -> Arc<Application> {
        self.app.clone()
    }
    /// Share only this verified owner's native write pipeline and existing queue.
    pub fn native_pipeline(&self) -> NativePipeline {
        NativePipeline::new(self.app.clone(), self.queue.native())
    }
    /// Owner-side control, never inferred from a socket filename or PID.
    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            shutdown: self.shutdown.clone(),
            paths: self.paths.clone(),
            instance: self.instance,
            diagnostics: self.log.sink(),
        }
    }
    /// Serve until explicit owner shutdown or the v1 idle condition is satisfied.
    pub async fn run(self) -> Result<()> {
        let Self {
            manager,
            project,
            app,
            listener,
            owner,
            options,
            shutdown,
            instance,
            delivery,
            log,
            commits,
            queue,
            ..
        } = self;
        let activity = Activity::new();
        let diagnostics = log.sink();
        let started = coordinator::start(
            app,
            manager,
            project.clone(),
            activity.clone(),
            coordinator::Configuration {
                instance,
                options,
                shutdown: shutdown.clone(),
                delivery,
                diagnostics: diagnostics.clone(),
                commits,
            },
            queue,
        )
        .await;
        let (service, worker) = match started {
            Ok(started) => started,
            Err(error) => {
                diagnostics.record(Event::MaintenanceFailed { code: error.code });
                let _ = log.close().await;
                return Err(error);
            }
        };
        diagnostics.record(Event::Started);
        let permits = Arc::new(Semaphore::new(32));
        let mut tasks = JoinSet::new();
        let mut stop = shutdown.subscribe();
        let mut tick = tokio::time::interval(options.housekeeping);
        // Check once after subscribing: a shutdown sent during coordinator
        // startup is already current and changed() would miss it. Later changes
        // are observed by the existing select, with no extra steady-state poll.
        let already_stopped = *stop.borrow();
        let result = if already_stopped {
            Ok(())
        } else {
            loop {
                tokio::select! {
                    changed=stop.changed()=>{if changed.is_err()||*stop.borrow() {break Ok(());}}
                    _=tick.tick()=>{if activity.expired(options.idle) {break Ok(());}}
                    accepted=listener.accept()=>match accepted {
                        Ok((stream,_))=>{
                            let Ok(permit)=permits.clone().try_acquire_owned() else {diagnostics.record(Event::ConnectionLimit);drop(stream);continue;};
                            let service=service.clone();let activity=activity.clone();let shutdown=stop.clone();
                            let project=project.identity().id;
                            tasks.spawn(async move {
                                let _permit=permit;
                                connection::serve(stream,service,activity,project,instance,options,shutdown).await;
                            });
                        }
                        Err(_)=>{diagnostics.record(Event::ListenerFailed);break Err(unavailable("Runtime listener failed."));},
                    },
                    Some(_)=tasks.join_next()=>{}
                }
            }
        };
        drop(listener);
        shutdown.send_replace(true);
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let stopped = service.stop().await;
        if let Err(error) = &stopped {
            diagnostics.record(Event::ShutdownFailed { code: error.code });
        }
        drop(service);
        let _ = worker.await;
        diagnostics.record(Event::Stopped);
        // Diagnostic failure is visible in status; it cannot undo application
        // cleanup or turn a successfully committed mutation into a failure.
        let _ = log.close().await;
        drop(owner);
        result.and(stopped)
    }
}
pub(super) fn busy(message: &str) -> Error {
    let mut e = Error::new(ErrorCode::Busy, message);
    e.retryable = true;
    e
}
pub(super) fn unavailable(message: &str) -> Error {
    let mut e = Error::new(ErrorCode::RuntimeUnavailable, message);
    e.retryable = true;
    e
}

/// Verify a live peer first, otherwise launch and wait for one checked owner.
/// Concurrent launchers may race; lifetime locks select the only daemon.
pub async fn connect_or_start<F, Fut>(
    paths: &RuntimePaths,
    project: Uuid,
    launch: F,
) -> Result<agentchat_protocol::rpc::Client>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    match agentchat_protocol::rpc::Client::connect(&paths.socket, project).await {
        Ok(client) => return Ok(client),
        Err(error) if error.code == ErrorCode::RuntimeUnavailable => {}
        Err(error) => return Err(error),
    }
    match launch().await {
        Ok(()) => {}
        Err(error) if error.code == ErrorCode::Busy => {}
        Err(error) => return Err(error),
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(
            deadline,
            agentchat_protocol::rpc::Client::connect(&paths.socket, project),
        )
        .await
        .map_err(|_| unavailable("Runtime did not become ready after launch."))?
        {
            Ok(client) => return Ok(client),
            Err(error) if error.code == ErrorCode::RuntimeUnavailable => {}
            Err(error) => return Err(error),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(unavailable("Runtime did not become ready after launch."));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
