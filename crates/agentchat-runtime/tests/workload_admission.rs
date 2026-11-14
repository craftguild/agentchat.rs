//! Saturated synthetic hint admission preserves finite capacity and original facts.
#[path = "workload/capture.rs"]
#[allow(dead_code)]
mod capture;
mod support;

use agentchat_core::{
    application::Mutation,
    delivery::{
        BatchState, DeliveryPort, HostReceipt, Presentation, SUBMISSION_SECONDS, WakeDispatch,
        WakeHint, WakeReference,
    },
    model::{Context, HostKind},
    ports::Storage,
    read::Query,
    sequence::Sequence,
};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::{
    context::ContextRegistry,
    daemon::{CommitObserver, Server},
};
use capture::{Capture, Input, SyntheticPort};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use support::runtime::{self, attach, mutate, options, query};
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

fn context() -> Context {
    Context {
        context_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        host: HostKind::Codex,
        host_session_id: "admission-only".into(),
        host_version: "fixture-only".into(),
        os_profile: "synthetic-not-admitted".into(),
        working_directory: "/synthetic".into(),
        incarnation: Uuid::new_v4(),
    }
}

fn dispatch(capture: &Capture, context: &Context, ordinal: i64) -> WakeDispatch {
    let request = Uuid::new_v4();
    capture.track(request);
    capture.committed(request, Sequence::new(ordinal * 2).unwrap());
    let message = Uuid::new_v4();
    WakeDispatch {
        hint: WakeHint {
            version: 1,
            project_id: context.project_id,
            context_id: context.context_id,
            incarnation: context.incarnation,
            agent_id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            events: vec![WakeReference {
                kind: "message.created".into(),
                delivery_id: Uuid::new_v4(),
                dispatch_id: message,
                message_id: message,
                seq: Sequence::new(ordinal * 2 - 1).unwrap(),
            }],
            pending_count: 1,
            preview: "one original hint".into(),
        },
        token: Uuid::new_v4(),
        attempt: 1,
        presentation: Presentation::Idle,
    }
}

fn exact(input: Input, expected: &WakeDispatch) {
    let Input::Native(actual) = input else {
        panic!("expected original native hint")
    };
    assert_eq!(actual.hint, expected.hint);
    assert_eq!(actual.token, expected.token);
    assert_eq!(actual.attempt, expected.attempt);
    assert_eq!(actual.presentation, expected.presentation);
}

async fn saturated(cancel: bool) {
    let context = context();
    let capture = Arc::new(Capture::new());
    capture.start(Instant::now());
    let (route, mut receive) = mpsc::channel(4);
    let port = SyntheticPort {
        capture: capture.clone(),
        routes: HashMap::from([(context.context_id, route.clone())]),
    };
    let originals = (1..=5)
        .map(|n| dispatch(&capture, &context, n))
        .collect::<Vec<_>>();
    for original in &originals[..4] {
        port.present(&context, original).await.unwrap();
    }
    let mut waiting = Box::pin(port.present(&context, &originals[4]));
    assert!(
        futures_util::poll!(&mut waiting).is_pending(),
        "full is temporary backpressure, not acceptance or panic"
    );
    assert_eq!(receive.len(), 4);
    assert_eq!(route.capacity(), 0);
    assert_eq!(capture.report(5)["max_model_queue"], 4);
    if cancel {
        assert!(
            tokio::time::timeout(Duration::from_secs(SUBMISSION_SECONDS), waiting)
                .await
                .is_err()
        );
        for original in &originals[..4] {
            exact(receive.recv().await.unwrap(), original);
        }
        assert!(
            receive.try_recv().is_err(),
            "cancelled admission must never enter later"
        );
    } else {
        exact(receive.recv().await.unwrap(), &originals[0]);
        waiting.await.unwrap();
        for original in &originals[1..] {
            exact(receive.recv().await.unwrap(), original);
        }
        assert!(
            receive.try_recv().is_err(),
            "each exact original is admitted once"
        );
    }
    assert_eq!(route.capacity(), 4);
    assert_eq!(capture.report(5)["max_model_queue"], 4);
}

#[tokio::test]
async fn full_mailbox_waits_then_admits_original_once_in_fifo() {
    saturated(false).await;
}

#[tokio::test]
async fn submission_deadline_cancels_wait_without_changing_original_queue() {
    saturated(true).await;
}

struct Observed {
    inner: SyntheticPort,
    entered: Notify,
    original: Mutex<Option<WakeDispatch>>,
}
#[async_trait::async_trait]
impl DeliveryPort for Observed {
    async fn present(
        &self,
        context: &Context,
        dispatch: &WakeDispatch,
    ) -> agentchat_core::Result<HostReceipt> {
        *self.original.lock().unwrap() = Some(dispatch.clone());
        self.entered.notify_one();
        self.inner.present(context, dispatch).await
    }
}

async fn start(client: &mut Client, context: &Context, name: &str) {
    success(
        client
            .call(
                Uuid::new_v4(),
                Call::StartLaunch {
                    context_id: context.context_id,
                    name: Some(name.into()),
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
}

async fn runtime_admission(cancel: bool) {
    let f = support::Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let registry = ContextRegistry::new(f.manager.clone());
    let sender = registry
        .issue(runtime::seed(&f, "admission-sender"))
        .await
        .unwrap();
    let recipient = registry
        .issue(runtime::seed(&f, "admission-recipient"))
        .await
        .unwrap();
    let host = Arc::new(support::native_host::NativeHost::default());
    host.bind(&sender);
    host.bind(&recipient);
    let capture = Arc::new(Capture::new());
    capture.start(Instant::now());
    let (route, mut receive) = mpsc::channel(4);
    for _ in 0..4 {
        assert!(route.try_send(Input::Stop).is_ok());
    }
    let observed = Arc::new(Observed {
        inner: SyntheticPort {
            capture: capture.clone(),
            routes: HashMap::from([(recipient.context_id, route)]),
        },
        entered: Notify::new(),
        original: Mutex::new(None),
    });
    let clock = Arc::new(runtime::ManualClock::default());
    let server = Server::bind(f.manager.clone(), project.clone(), host, clock, options())
        .await
        .unwrap()
        .with_delivery(observed.clone())
        .with_commit_observer(capture);
    let app = server.application();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let mut client = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    let mut target = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    start(&mut client, &sender, "alice").await;
    start(&mut target, &recipient, "bob").await;
    attach(&mut target, &recipient).await;
    let request = Uuid::new_v4();
    observed.inner.capture.track(request);
    success(
        client
            .call(
                request,
                Call::Mutate {
                    request_id: request,
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: sender.context_id,
                    },
                    command: Mutation::Post {
                        channel: Some("dev".into()),
                        body: "@bob exact original".into(),
                    },
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), observed.entered.notified())
        .await
        .unwrap();
    let original = observed.original.lock().unwrap().clone().unwrap();
    assert_eq!(receive.len(), 4);
    tokio::time::timeout(
        Duration::from_secs(SUBMISSION_SECONDS - 1),
        mutate(
            &mut client,
            &sender,
            Mutation::ChannelCreate {
                name: "writer-progress".into(),
            },
        ),
    )
    .await
    .unwrap();
    let stored = f.manager.open(&project).await.unwrap();
    let batch = stored
        .begin()
        .await
        .unwrap()
        .wake_batch(original.hint.batch_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.state, BatchState::Pending);
    assert_eq!(batch.dispatch_ref, Some(original.token));
    assert_eq!(batch.attempts, original.attempt);
    if cancel {
        tokio::time::timeout(Duration::from_secs(SUBMISSION_SECONDS + 2), async {
            loop {
                let batch = stored
                    .begin()
                    .await
                    .unwrap()
                    .wake_batch(original.hint.batch_id)
                    .await
                    .unwrap()
                    .unwrap();
                if batch.state == BatchState::Inflight {
                    assert_eq!(batch.failure_code.as_deref(), Some("runtime_unavailable"));
                    assert_eq!(batch.dispatch_ref, Some(original.token));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        for _ in 0..4 {
            assert!(matches!(receive.recv().await, Some(Input::Stop)));
        }
        assert!(receive.try_recv().is_err());
    } else {
        assert!(matches!(receive.recv().await, Some(Input::Stop)));
        for _ in 0..3 {
            assert!(matches!(receive.recv().await, Some(Input::Stop)));
        }
        exact(
            tokio::time::timeout(Duration::from_secs(2), receive.recv())
                .await
                .unwrap()
                .unwrap(),
            &original,
        );
        assert!(receive.try_recv().is_err());
    }
    let message = original.hint.events[0].message_id;
    let detail = query(
        &mut client,
        None,
        Query::Message {
            message_id: message,
            receipts: Default::default(),
        },
    )
    .await;
    assert!(
        detail["receipts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["delivery"]["delivered_at"].is_null()
                && row["reads"].as_array().unwrap().is_empty())
    );
    if !cancel {
        runtime::accept_native(&app, &recipient, vec![original.hint.events[0].delivery_id]).await;
        let detail = query(
            &mut client,
            None,
            Query::Message {
                message_id: message,
                receipts: Default::default(),
            },
        )
        .await;
        assert!(
            detail["receipts"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| !row["delivery"]["delivered_at"].is_null())
        );
    }
    handle.shutdown();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn admission_wait_releases_rpc_writer_and_keeps_full_input_fact_separate() {
    runtime_admission(false).await;
}

#[tokio::test]
async fn runtime_submission_timeout_preserves_original_token_without_admitting_hint() {
    runtime_admission(true).await;
}
