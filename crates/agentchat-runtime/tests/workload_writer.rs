//! The hint consumer uses the verified runtime's writer, not another SQLite pool.
mod support;

use agentchat_core::{
    ErrorCode, Result,
    application::{Application, Mutation},
    model::{Context, LaunchIdentity},
    ports::{HostGate, HostState},
    read::Query,
    time::SystemClock,
};
use agentchat_protocol::rpc::{ActorReference, Call, Client, success};
use agentchat_runtime::{
    context::ContextRegistry,
    daemon::{RuntimeOptions, Server},
};
use std::{sync::Arc, time::Duration};
use support::{Fixture, SyntheticHost, runtime};
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Default)]
struct HeldJoin {
    entered: Notify,
    release: Notify,
}

#[async_trait::async_trait]
impl HostGate for HeldJoin {
    fn input_capabilities(
        &self,
        context: &Context,
    ) -> Result<agentchat_core::delivery::InputCapabilities> {
        SyntheticHost.input_capabilities(context)
    }

    fn allows_unmanaged_participation(&self) -> bool {
        true
    }

    async fn verify(&self, context: &Context) -> Result<HostState> {
        SyntheticHost.verify(context).await
    }

    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        if context.host_session_id == "held-writer" {
            // Join invokes this after its immediate transaction starts. This
            // intentional hold distinguishes connection admission from SQLite
            // busy handling; it is not a claim about the original workload's
            // unobserved lock holder or a change to production timeouts.
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(None)
    }
}

async fn held_writer(shared: bool) {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let registry = ContextRegistry::new(f.manager.clone());
    let reader = registry
        .issue(runtime::seed(&f, "rpc-writer"))
        .await
        .unwrap();
    let holder = registry
        .issue(runtime::seed(&f, "held-writer"))
        .await
        .unwrap();
    let gate = Arc::new(HeldJoin::default());
    let server = Server::bind(
        f.manager.clone(),
        project.clone(),
        gate.clone(),
        Arc::new(SystemClock),
        RuntimeOptions {
            housekeeping: Duration::from_secs(10),
            ..RuntimeOptions::default()
        },
    )
    .await
    .unwrap();
    let consumer = if shared {
        server.application()
    } else {
        Arc::new(Application::new(
            f.manager.open(&project).await.unwrap(),
            Arc::new(SystemClock),
            gate.clone(),
        ))
    };
    let handle = server.handle();
    let server_task = tokio::spawn(server.run());
    let mut client = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    runtime::join(&mut client, &reader, "rpc-writer").await;
    let held = tokio::spawn(async move {
        consumer
            .execute(
                Uuid::new_v4(),
                &holder,
                Mutation::Join {
                    name: Some("holder".into()),
                },
            )
            .await
    });
    gate.entered.notified().await;
    let key = Uuid::new_v4();
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: "one original operation".into(),
    };
    let mut pending = Box::pin(client.call(
        key,
        Call::Mutate {
            request_id: key,
            actor: ActorReference::Native {
                context_id: reader.context_id,
            },
            command: command.clone(),
        },
    ));
    let submitted = std::time::Instant::now();
    let first = if shared {
        // The holder stays locked throughout this artificial observation.
        // A queued call must remain unfinished, then succeed after release.
        tokio::time::timeout(Duration::from_millis(5_500), &mut pending).await
    } else {
        // RPC submission precedes coordinator dispatch and SQLite BEGIN.
        // Wait for the actual Busy response before releasing the writer;
        // do not treat elapsed time from submission as SQLite's5s clock.
        // This test-only watchdog covers the Client's existing5s write and
        //30s response bounds. It is not a product latency acceptance target.
        tokio::time::timeout(Duration::from_secs(35), &mut pending).await
    };
    let busy_observed = matches!(&first, Ok(Err(error)) if error.code == ErrorCode::Busy);
    eprintln!(
        "writer control shared={shared} rpc_wait_ms={} busy_before_release={busy_observed}",
        submitted.elapsed().as_secs_f64() * 1000.0
    );
    gate.release.notify_one();
    held.await.unwrap().unwrap();
    let was_queued = first.is_err();
    let reply = match first {
        Ok(reply) => {
            drop(pending);
            reply
        }
        Err(_) => pending.await,
    };
    if shared {
        assert!(was_queued, "RPC must remain queued behind the held writer");
        success(reply.unwrap()).unwrap();
    } else {
        assert!(
            busy_observed,
            "independent writer must report Busy before release within the RPC watchdog"
        );
        assert_eq!(reply.unwrap_err().code, ErrorCode::Busy);
    }
    let stats = runtime::query(&mut client, None, Query::Whoami).await;
    assert_eq!(stats["statistics"]["messages"], u64::from(shared));
    // The failed independent write produced no successful fact. Only after
    // positive release may this exact semantic operation be issued again.
    if !shared {
        let reply = client
            .call(
                key,
                Call::Mutate {
                    request_id: key,
                    actor: ActorReference::Native {
                        context_id: reader.context_id,
                    },
                    command,
                },
            )
            .await
            .unwrap();
        success(reply).unwrap();
        let stats = runtime::query(&mut client, None, Query::Whoami).await;
        assert_eq!(stats["statistics"]["messages"], 1);
    }
    handle.shutdown();
    server_task.await.unwrap().unwrap();
    let store = f.manager.open(&project).await.unwrap();
    store.integrity_check().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_owner_application_queues_rpc_without_starting_a_competing_sqlite_writer() {
    held_writer(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_application_retains_busy_timeout_and_does_not_commit_failed_mutation() {
    held_writer(false).await;
}
