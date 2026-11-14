//! Mixed ready writes preserve exact callbacks, semantic keys and outer durability.
mod support;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::{
        Application, CommitOutcome, CommitRequest, MAX_COMMIT_GROUP, Mutation, MutationRequest,
    },
    delivery::{BatchState, HostReceipt, MessageCompletion, WakeCompletion, WakeDispatch},
    model::{Context, DeliveryState},
    ports::{HostGate, HostState, Storage, Transaction},
};
use agentchat_store::Store;
use async_trait::async_trait;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{Fixture, SyntheticHost, id, native::Gate as NativeGate};
use tokio::sync::Notify;
use uuid::Uuid;

fn mutation(context: &Context, command: Mutation) -> MutationRequest {
    MutationRequest {
        request_id: Uuid::new_v4(),
        context: context.clone(),
        command,
    }
}
fn post(body: &str) -> Mutation {
    Mutation::Post {
        channel: Some("dev".into()),
        body: body.into(),
    }
}
fn completion(context: &Context, dispatch: &WakeDispatch) -> WakeCompletion {
    WakeCompletion {
        context: context.clone(),
        dispatch: dispatch.clone(),
        outcome: Ok(HostReceipt {
            turn_id: Some("native-turn".into()),
        }),
    }
}
struct Counted {
    store: Arc<Store>,
    begins: AtomicUsize,
}
#[async_trait]
impl Storage for Counted {
    fn project_id(&self) -> Uuid {
        self.store.project_id()
    }
    async fn snapshot(&self) -> Result<Box<dyn agentchat_core::read::Snapshot>> {
        self.store.snapshot().await
    }
    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        self.begins.fetch_add(1, Ordering::SeqCst);
        self.store.begin().await
    }
}

#[tokio::test]
async fn ordered_native_completion_and_explicit_read_share_one_writer_independently() {
    for callback_first in [true, false] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        let gate = NativeGate::new(&f);
        gate.bind(&b);
        let initial = gate.app(&f);
        initial.start_launch(&b, Some("bob")).await.unwrap();
        initial
            .initialize_with_settings(
                None,
                agentchat_core::project::ReceiptSettingsUpdate {
                    auto_read: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let message = id(&f.post(&a, "@bob original").await.result, "message_id");
        let dispatch = initial.prepare_message(&b).await.unwrap().unwrap();
        let read = mutation(
            &b,
            Mutation::Read {
                message_ids: vec![message],
            },
        );
        let store = Arc::new(Counted {
            store: f.store.clone(),
            begins: AtomicUsize::new(0),
        });
        let app = Application::new(store.clone(), f.clock.clone(), gate);
        let callback = CommitRequest::MessageCompletion(MessageCompletion {
            context: b.clone(),
            dispatch: dispatch.clone(),
            outcome: Ok(agentchat_core::delivery::MessageProgress::Accepted(
                HostReceipt::default(),
            )),
        });
        let mut requests = vec![callback, CommitRequest::Mutation(read.clone())];
        if !callback_first {
            requests.reverse();
        }
        let mut outcomes = app.commit_ready(requests).await.unwrap();
        assert_eq!(store.begins.load(Ordering::SeqCst), 1);
        if !callback_first {
            outcomes.reverse();
        }
        assert!(matches!(
            outcomes[0],
            Ok(CommitOutcome::MessageCompletion(true))
        ));
        let original = match outcomes.pop().unwrap().unwrap() {
            CommitOutcome::Mutation(value) => value.result,
            _ => panic!("missing explicit-read result"),
        };
        assert_eq!(
            app.execute(read.request_id, &read.context, read.command)
                .await
                .unwrap()
                .result,
            original
        );
        let mut tx = f.store.begin().await.unwrap();
        let delivery = tx
            .delivery(dispatch.input.delivery_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.state, DeliveryState::Delivered);
        assert!(delivery.history.delivered_at.is_some());
        tx.rollback().await.unwrap();
        f.assert_read_kinds(0, 1).await;
        assert!(
            !app.finish_message(&b, &dispatch, Ok(HostReceipt::default()))
                .await
                .unwrap()
        );
        assert!(app.prepare_message(&b).await.unwrap().is_none());
        f.store.integrity_check().await.unwrap();
    }
}

#[tokio::test]
async fn malformed_callback_and_failed_mutation_do_not_undo_independent_ready_writes() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    f.post(&a, "@bob original").await;
    let dispatch = f.app.prepare_wake(&b).await.unwrap().unwrap();
    let first = mutation(&a, post("successful key"));
    let failed = mutation(
        &a,
        Mutation::Reply {
            read_message_ids: Vec::new(),
            root_message_id: Uuid::nil(),
            body: "invalid root".into(),
        },
    );
    let mut bad = completion(&b, &dispatch);
    bad.outcome = Ok(HostReceipt {
        turn_id: Some(String::new()),
    });
    let mut conflict = first.clone();
    conflict.command = post("different arguments");
    let results = f
        .app
        .commit_ready(vec![
            CommitRequest::WakeCompletion(bad),
            CommitRequest::Mutation(first.clone()),
            CommitRequest::Mutation(failed.clone()),
            CommitRequest::WakeCompletion(completion(&b, &dispatch)),
            CommitRequest::Mutation(first.clone()),
            CommitRequest::Mutation(conflict),
            CommitRequest::WakeCompletion(completion(&b, &dispatch)),
            CommitRequest::Mutation(mutation(&b, post("independent actor"))),
        ])
        .await
        .unwrap();
    assert_eq!(results.len(), MAX_COMMIT_GROUP);
    assert_eq!(
        results[0].as_ref().unwrap_err().code,
        ErrorCode::IntegrityError
    );
    assert_eq!(
        results[2].as_ref().unwrap_err().code,
        ErrorCode::InvalidThread
    );
    assert!(matches!(
        results[3],
        Ok(CommitOutcome::WakeCompletion(true))
    ));
    assert!(matches!(
        results[6],
        Ok(CommitOutcome::WakeCompletion(false))
    ));
    assert_eq!(
        results[5].as_ref().unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    for i in [1, 4, 7] {
        assert!(matches!(results[i], Ok(CommitOutcome::Mutation(_))));
    }
    assert_eq!(f.count("dispatch_messages").await, 3);
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.idempotency(failed.request_id).await.unwrap().is_none());
    assert_eq!(
        tx.wake_batch(dispatch.hint.batch_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        BatchState::Inflight
    );
    tx.rollback().await.unwrap();
    assert_eq!(f.count("read_receipts").await, 0);
    f.store.integrity_check().await.unwrap();
}

struct Gate {
    inner: Arc<SyntheticHost>,
    actor: Uuid,
    entered: Notify,
    release: Notify,
    failure: Option<ErrorCode>,
}
#[async_trait]
impl HostGate for Gate {
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
        if context.context_id == self.actor {
            if let Some(code) = self.failure {
                return Err(Error::new(code, "Injected storage failure."));
            }
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.verify(context).await
    }
}

#[tokio::test]
async fn callback_savepoint_remains_invisible_and_cancellation_recovers_original_claim_and_key() {
    for cancel in [false, true] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        f.join(&b, "bob").await;
        f.post(&a, "@bob original").await;
        let dispatch = f.app.prepare_wake(&b).await.unwrap().unwrap();
        let request = mutation(&a, post("held mutation"));
        let gate = Arc::new(Gate {
            inner: f.host.clone(),
            actor: a.context_id,
            entered: Notify::new(),
            release: Notify::new(),
            failure: None,
        });
        let app = Arc::new(Application::new(
            f.store.clone(),
            f.clock.clone(),
            gate.clone(),
        ));
        let inputs = vec![
            CommitRequest::WakeCompletion(completion(&b, &dispatch)),
            CommitRequest::Mutation(request.clone()),
        ];
        let task = tokio::spawn({
            let app = app.clone();
            let inputs = inputs.clone();
            async move { app.commit_ready(inputs).await }
        });
        tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
            .await
            .unwrap();
        let batch = f
            .rows("wake_batches")
            .await
            .into_iter()
            .find(|r| r.try_get::<String>("", "id").unwrap() == dispatch.hint.batch_id.to_string())
            .unwrap();
        assert_eq!(batch.try_get::<String>("", "state").unwrap(), "pending");
        assert_eq!(f.count("dispatch_messages").await, 1);
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            gate.release.notify_one();
            assert!(task.await.unwrap().unwrap().iter().all(Result::is_ok));
        }
        assert_eq!(
            f.app
                .finish_wake(&b, &dispatch, Ok(HostReceipt::default()))
                .await
                .unwrap(),
            cancel
        );
        let retried = f
            .app
            .execute(request.request_id, &request.context, request.command)
            .await
            .unwrap();
        assert_eq!(retried.event_head.is_some(), cancel);
        assert_eq!(f.count("dispatch_messages").await, 2);
        assert_eq!(f.count("read_receipts").await, 0);
    }
}

#[tokio::test]
async fn infrastructure_errors_abort_both_kinds_and_invalid_groups_never_reserve_a_writer() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    f.post(&a, "@bob original").await;
    let dispatch = f.app.prepare_wake(&b).await.unwrap().unwrap();
    for code in [
        ErrorCode::StorageError,
        ErrorCode::IoError,
        ErrorCode::IntegrityError,
    ] {
        let gate = Arc::new(Gate {
            inner: f.host.clone(),
            actor: a.context_id,
            entered: Notify::new(),
            release: Notify::new(),
            failure: Some(code),
        });
        let app = Application::new(f.store.clone(), f.clock.clone(), gate);
        let request = mutation(&a, post("must roll back"));
        assert_eq!(
            app.commit_ready(vec![
                CommitRequest::WakeCompletion(completion(&b, &dispatch)),
                CommitRequest::Mutation(request.clone())
            ])
            .await
            .unwrap_err()
            .code,
            code
        );
        let mut tx = f.store.begin().await.unwrap();
        assert_eq!(
            tx.wake_batch(dispatch.hint.batch_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            BatchState::Pending
        );
        assert!(tx.idempotency(request.request_id).await.unwrap().is_none());
        tx.rollback().await.unwrap();
    }
    let held = f.store.begin().await.unwrap();
    let mut foreign = completion(&b, &dispatch);
    foreign.context.project_id = Uuid::new_v4();
    for inputs in [
        vec![],
        vec![CommitRequest::WakeCompletion(foreign.clone()); MAX_COMMIT_GROUP + 1],
        vec![
            CommitRequest::WakeCompletion(foreign),
            CommitRequest::Mutation(mutation(&a, post(""))),
        ],
    ] {
        let result = tokio::time::timeout(Duration::from_millis(500), f.app.commit_ready(inputs))
            .await
            .unwrap();
        if let Ok(results) = result {
            assert!(results.iter().all(Result::is_err));
        } else {
            assert_eq!(result.unwrap_err().code, ErrorCode::InvalidArgument);
        }
    }
    held.rollback().await.unwrap();
    assert_eq!(f.count("dispatch_messages").await, 1);
}
