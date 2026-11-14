//! One bounded application coordinator and coalesced post-commit observer hints.

mod checks;
mod execute;
mod ingress;
mod maintenance;
mod mutations;
mod native;
use ingress::Command;
pub(super) use ingress::{NativeIngress, NativeRequest, Queue};

use super::{RuntimeOptions, activity::Activity, delivery::Dispatches};
use crate::{
    context::ContextRegistry,
    diagnostics::{Event, Sink},
    project::{Project, ProjectManager},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    application::{Application, MAX_COMMIT_GROUP},
    model::Context,
    sequence::Cursor,
};
use agentchat_protocol::rpc::Call;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

pub(super) struct Subscription {
    pub kind: FollowKind,
    pub hints: watch::Receiver<Cursor>,
}
pub(super) enum FollowKind {
    Changes {
        after: agentchat_core::read::Checkpoint,
    },
    Hold,
}
#[derive(serde::Serialize)]
#[serde(untagged)]
pub(super) enum Response {
    Read(Box<agentchat_protocol::rpc::ReadResult>),
    Other(Value),
    NativeBinding(Box<agentchat_core::read::BindingView>),
    InternalHealth(Box<agentchat_protocol::health::InternalHealth>),
    ChangeStart(Box<agentchat_protocol::rpc::ChangeStart>),
    ObserverStart(Box<agentchat_protocol::rpc::ObserverStart>),
}
impl From<Value> for Response {
    fn from(value: Value) -> Self {
        Self::Other(value)
    }
}
pub(super) struct Answer {
    pub result: Response,
    pub subscription: Option<Subscription>,
    pub bind: bool,
}
#[derive(Clone)]
pub(super) struct Service {
    sender: mpsc::Sender<Command>,
    checks: Arc<checks::Checks>,
    diagnostics: Sink,
}
impl Service {
    pub async fn read<R: agentchat_core::read::ReadRequest>(
        &self,
        connection: Uuid,
        context_id: Option<Uuid>,
        request: R,
        bound: Option<Uuid>,
    ) -> Result<R::Response<agentchat_protocol::health::RuntimeWhoami>> {
        let answer = self
            .call(
                connection,
                None,
                Call::Query {
                    actor: context_id.map(|context_id| {
                        agentchat_protocol::rpc::ActorReference::Native { context_id }
                    }),
                    query: request.into_query(),
                },
                bound,
            )
            .await?;
        let Response::Read(result) = answer.result else {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "Read dispatch returned a non-read result.",
            ));
        };
        Ok(R::from_result(*result))
    }

    pub async fn call(
        &self,
        connection: Uuid,
        request_id: Option<Uuid>,
        call: Call,
        bound: Option<Uuid>,
    ) -> Result<Answer> {
        if matches!(
            &call,
            Call::CheckDelivery { .. } | Call::CheckMessage { .. } | Call::CheckNative { .. }
        ) {
            // Bounded socket handlers may read independently. A command can be
            // awaiting the native callback that is itself making this check;
            // enqueueing it would create a circular native/RPC wait. This path
            // invokes only the shared read-only application policy, with no
            // writer, native getter, binding or dispatch scheduling.
            if self.sender.is_closed() {
                return Err(super::unavailable("Runtime coordinator stopped."));
            }
            let started = Instant::now();
            let result = self.checks.execute(&call, bound).await;
            self.diagnostics.record(Event::RpcTiming {
                request_id,
                queue_us: 0,
                execution_us: crate::diagnostics::elapsed(started),
            });
            return result;
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Command::Call {
                request_id,
                queued_at: Instant::now(),
                connection,
                call: Box::new(call),
                bound,
                reply,
            })
            .map_err(|_| super::busy("Runtime request queue is full."))?;
        receive
            .await
            .map_err(|_| super::unavailable("Runtime coordinator stopped."))?
    }
    pub async fn gone(&self, connection: Uuid) {
        // Bounded connections imply bounded disconnect messages. Waiting here
        // preserves cleanup when the normal request queue is briefly saturated.
        let _ = self.sender.send(Command::Gone(connection)).await;
    }
    pub async fn stop(&self) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::Stop(reply))
            .await
            .map_err(|_| super::unavailable("Runtime coordinator is closed."))?;
        receive
            .await
            .map_err(|_| super::unavailable("Runtime shutdown result was lost."))?
    }
}
struct Worker {
    app: Arc<Application>,
    manager: ProjectManager,
    project: Project,
    registry: ContextRegistry,
    heads: watch::Sender<Cursor>,
    activity: Activity,
    attached: HashMap<Uuid, Context>,
    pending: HashMap<Uuid, Context>,
    last_error: Option<ErrorCode>,
    instance: Uuid,
    deliveries: Dispatches,
    staging: agentchat_hosts::staging::OriginReaper,
    diagnostics: Sink,
    commits: Option<Arc<dyn super::CommitObserver>>,
    grouping: mutations::Metrics,
    native_jobs_completed: bool,
    native_empty_at: Option<Cursor>,
}
pub(super) struct Configuration {
    pub instance: Uuid,
    pub options: RuntimeOptions,
    pub shutdown: watch::Sender<bool>,
    pub delivery: Option<super::delivery::DispatchPort>,
    pub diagnostics: Sink,
    pub commits: Option<Arc<dyn super::CommitObserver>>,
}
pub(super) async fn start(
    app: Arc<Application>,
    manager: ProjectManager,
    project: Project,
    activity: Activity,
    configuration: Configuration,
    queue: Queue,
) -> Result<(Service, tokio::task::JoinHandle<()>)> {
    let Configuration {
        instance,
        options,
        shutdown,
        delivery,
        diagnostics,
        commits,
    } = configuration;
    app.invalidate_connections().await?;
    app.maintain().await?;
    let (heads, _) = watch::channel(app.event_head().await?);
    let native_pipeline = super::NativePipeline::new(app.clone(), queue.native());
    let (sender, mut receiver) = queue.into_parts();
    let checks = Arc::new(checks::Checks::new(
        app.clone(),
        manager.clone(),
        project.clone(),
        shutdown.subscribe(),
    ));
    let staging = agentchat_hosts::staging::OriginReaper::new(manager.home())
        .with_context_validator(ContextRegistry::validate_stage);
    let mut worker = Worker {
        app,
        registry: ContextRegistry::new(manager.clone()),
        manager,
        project,
        activity,
        heads,
        attached: HashMap::new(),
        pending: HashMap::new(),
        last_error: None,
        instance,
        deliveries: Dispatches::new(delivery, diagnostics.clone())
            .with_preparation(Arc::new(native_pipeline)),
        staging,
        diagnostics: diagnostics.clone(),
        commits,
        grouping: mutations::Metrics::default(),
        native_jobs_completed: false,
        native_empty_at: None,
    };
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(options.housekeeping);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut carried = None;
        loop {
            let schedule = tokio::select! {
                command=async { if let Some(command)=carried.take() {Some(command)} else {receiver.recv().await} }=>match command {
                    Some(Command::Call {connection,call,bound,reply,request_id,queued_at})=>{
                        // Keep queue slots compact as typed read requests grow;
                        // dispatch still consumes one exact call in FIFO order.
                        let call=*call;
                        if call.grouped_mutation() {
                            let pending=mutations::collect(Some(mutations::Pending::Mutation(mutations::MutationPending {call,bound,reply,request_id,queued_at})), &mut receiver, &mut carried, MAX_COMMIT_GROUP);
                            let finished=worker.take_native_ready(None, MAX_COMMIT_GROUP-pending.len());
                            worker.commit_group(pending,finished,false).await
                        } else {
                            let changes_work = !matches!(&call,Call::Query {..}|Call::NativeBinding {..}|Call::InternalHealth|Call::SubscribeChanges {..}|Call::Observe|Call::CheckDelivery {..}|Call::CheckMessage {..}|Call::CheckNative {..});
                            let queue_us=crate::diagnostics::elapsed(queued_at);
                            let started=Instant::now();
                            let result=worker.execute(connection,call,bound).await;
                            worker.diagnostics.record(Event::RpcTiming {request_id,queue_us,execution_us:crate::diagnostics::elapsed(started)});
                            let schedule=changes_work && result.is_ok();
                            worker.diagnostics.record(match &result {Ok(_)=>Event::RequestSucceeded,Err(error)=>Event::RequestFailed {code:error.code}});
                            let _=reply.send(result);
                            schedule
                        }
                    }
                    Some(Command::Native(pending))=>{
                        let pending=mutations::collect(Some(mutations::Pending::Native(*pending)), &mut receiver, &mut carried, MAX_COMMIT_GROUP);
                        let finished=worker.take_native_ready(None, MAX_COMMIT_GROUP-pending.len());
                        worker.commit_group(pending,finished,false).await
                    },
                    Some(Command::Gone(connection))=>{
                        let participant = worker.attached.contains_key(&connection);
                        worker.remove(connection);
                        // One launch normally owns both a supervisor and a
                        // native MCP connection.  If either closes after the
                        // physical owner is gone, reconcile the surviving
                        // attachment before another native dispatch scan.
                        // An observer/unbound query socket has no native
                        // lifecycle to reconcile. Its close must remain a
                        // read-only operation, including behind another writer.
                        if participant { worker.maintain().await; }
                        participant
                    },
                    Some(Command::Stop(reply))=>{
                        let result=worker.app.invalidate_connections().await;let _=reply.send(result);break;
                    }
                    None=>{let _=worker.app.invalidate_connections().await;break;}
                },
                Some(finished)=worker.deliveries.receiver.recv()=>{
                    let finished=worker.take_native_ready(Some(finished), MAX_COMMIT_GROUP);
                    let pending=mutations::collect(None, &mut receiver, &mut carried, MAX_COMMIT_GROUP-finished.len());
                    worker.commit_group(pending,finished,true).await
                },
                _=interval.tick()=>{
                    if worker.manager.validate(&worker.project).await.is_err() {
                        worker.diagnostics.record(Event::ProjectInvalid);
                        let _=worker.app.invalidate_connections().await;
                        shutdown.send_replace(true);break;
                    }
                    worker.maintain().await;
                    true
                }
            };
            // Observation cannot add or release wake work. Avoid entering an
            // immediate writer transaction per participant after every read.
            // Mutations, lifecycle changes, completions and the independent
            // housekeeping timer still schedule immediately; queries cannot
            // starve retries, expiry notices or reconnect delivery.
            if schedule && worker.pending.is_empty() {
                let contexts = worker.attached.values().cloned().collect();
                let head = *worker.heads.borrow();
                if let Err(error) = worker
                    .deliveries
                    .start_at(&worker.app, contexts, head)
                    .await
                {
                    worker.control_error(Event::NativeFailed { code: error.code }, error.code);
                }
            }
            worker.activity.participants(worker.attached.len());
        }
    });
    Ok((
        Service {
            sender,
            checks,
            diagnostics,
        },
        task,
    ))
}
impl Worker {
    fn control_error(&mut self, event: Event, code: ErrorCode) {
        self.last_error = Some(code);
        self.diagnostics.record(event);
    }
    fn remove(&mut self, connection: Uuid) {
        if let Some(context) = self.attached.remove(&connection)
            && !self
                .attached
                .values()
                .any(|c| c.context_id == context.context_id)
        {
            self.deliveries.cancel(context.context_id);
            self.pending.insert(context.context_id, context);
        }
    }
    async fn resolve_context(&self, call: &Call, bound: Option<Uuid>) -> Result<Option<Context>> {
        if call.selects_user() && bound.is_some() {
            return Err(Error::new(
                ErrorCode::IdentityConflict,
                "A native-bound connection cannot switch to the anonymous user.",
            ));
        }
        checks::context(
            &self.manager,
            &self.project,
            &self.registry,
            call.context_id(),
            bound,
        )
        .await
    }
}

fn session() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "An exact registered host context is required.",
    )
}

#[cfg(test)]
#[path = "../../tests/unit/native_rearm.rs"]
mod native_rearm_tests;
