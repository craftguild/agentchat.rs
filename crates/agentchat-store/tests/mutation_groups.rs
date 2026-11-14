//! Savepoint isolation, outer durability, cancellation and original retry results.
mod support;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::{Application, MAX_MUTATION_GROUP, Mutation, MutationRequest},
    model::{Context, Phase},
    ports::{HostGate, HostState, Storage},
};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use support::{Fixture, SyntheticHost};
use tokio::sync::Notify;
use uuid::Uuid;

fn request(context: &Context, command: Mutation) -> MutationRequest {
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

#[tokio::test]
async fn grouped_order_retries_identity_leave_and_failed_keys_match_single_policy() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let first = request(&a, post("@bob original immutable result"));
    let mut conflict = first.clone();
    conflict.command = post("different");
    let leave = request(&a, Mutation::Leave);
    let after_leave = request(&a, post("must fail after leave"));
    let mut other_actor = first.clone();
    other_actor.context = b.clone();
    let results = f
        .app
        .execute_group(vec![
            first.clone(),
            first.clone(),
            conflict,
            leave,
            first.clone(),
            after_leave.clone(),
            other_actor,
            request(&b, post("bob remains an independent actor")),
        ])
        .await
        .unwrap();
    let original = results[0].as_ref().unwrap();
    for i in [1, 4] {
        assert_eq!(results[i].as_ref().unwrap().result, original.result);
        assert!(results[i].as_ref().unwrap().event_head.is_none());
    }
    assert_eq!(
        results[2].as_ref().unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(results[5].as_ref().unwrap_err().code, ErrorCode::NotJoined);
    assert_eq!(
        results[6].as_ref().unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    assert!(results[7].is_ok());
    assert_eq!(f.count("dispatch_messages").await, 2);
    assert!(
        f.store
            .begin()
            .await
            .unwrap()
            .idempotency(after_leave.request_id)
            .await
            .unwrap()
            .is_none()
    );
    let before = f.counts().await;
    f.host.phase(&a, Phase::Disconnected);
    assert_eq!(
        f.app
            .execute(first.request_id, &a, first.command)
            .await
            .unwrap()
            .result,
        original.result
    );
    assert_eq!(f.counts().await, before);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn conflicts_with_prior_staged_leases_rollback_only_the_failed_request() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let reserve = |patterns: &[&str]| Mutation::Reserve {
        patterns: patterns.iter().map(|s| (*s).into()).collect(),
        ttl_seconds: Some(30),
        reason: None,
    };
    let failed = request(&b, reserve(&["independent/**", "src/**"]));
    let successful = request(&b, reserve(&["independent/**"]));
    let result = f
        .app
        .execute_group(vec![
            request(&a, reserve(&["src/**"])),
            failed.clone(),
            successful.clone(),
        ])
        .await
        .unwrap();
    assert_eq!(
        result[1].as_ref().unwrap_err().code,
        ErrorCode::ReservationConflict
    );
    assert_eq!(
        result[1].as_ref().unwrap_err().details["conflicts"][0]["owner"]["handle"],
        "alice"
    );
    assert!(result[2].is_ok());
    assert_eq!(f.count("reservations").await, 2);
    assert!(
        f.store
            .begin()
            .await
            .unwrap()
            .idempotency(failed.request_id)
            .await
            .unwrap()
            .is_none()
    );
    // A failed operation must also undo expiry events staged before its later
    // domain validation fails. The following success materializes expiry once.
    f.clock.advance(31);
    let failed = request(
        &a,
        Mutation::Reply {
            read_message_ids: Vec::new(),
            root_message_id: Uuid::nil(),
            body: "missing root".into(),
        },
    );
    let results = f
        .app
        .execute_group(vec![failed.clone(), request(&a, post("after expiry"))])
        .await
        .unwrap();
    assert_eq!(
        results[0].as_ref().unwrap_err().code,
        ErrorCode::InvalidThread
    );
    assert_eq!(
        f.rows("events")
            .await
            .iter()
            .filter(|r| r.try_get::<String>("", "kind").unwrap() == "reservation.expired")
            .count(),
        2
    );
    assert!(
        f.store
            .begin()
            .await
            .unwrap()
            .idempotency(failed.request_id)
            .await
            .unwrap()
            .is_none()
    );
    f.store.integrity_check().await.unwrap();
}

struct BlockSecond {
    inner: Arc<SyntheticHost>,
    target: Uuid,
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl HostGate for BlockSecond {
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
        if context.context_id == self.target {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.verify(context).await
    }
}
#[tokio::test]
async fn savepoint_success_is_invisible_until_outer_commit_and_cancellation_rolls_back_all() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let gate = Arc::new(BlockSecond {
        inner: f.host.clone(),
        target: b.context_id,
        entered: Notify::new(),
        release: Notify::new(),
    });
    let app = Arc::new(Application::new(
        f.store.clone(),
        f.clock.clone(),
        gate.clone(),
    ));
    for cancelled in [false, true] {
        let before = f.counts().await;
        let requests = vec![
            request(&a, post("@bob held first")),
            request(&b, post("@alice held second")),
        ];
        let task = tokio::spawn({
            let app = app.clone();
            let requests = requests.clone();
            async move { app.execute_group(requests).await }
        });
        tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
            .await
            .unwrap();
        // The first child has already released its savepoint; an independent
        // reader still sees none of that group's aggregate/event/inbox/keys.
        assert_eq!(f.counts().await, before);
        assert!(!task.is_finished());
        if cancelled {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            let mut fence = f.store.begin().await.unwrap();
            for r in &requests {
                assert!(fence.idempotency(r.request_id).await.unwrap().is_none());
            }
            fence.rollback().await.unwrap();
            assert_eq!(f.counts().await, before);
        } else {
            gate.release.notify_one();
            let results = task.await.unwrap().unwrap();
            assert!(results.iter().all(Result::is_ok));
        }
        for r in requests {
            let result = f
                .app
                .execute(r.request_id, &r.context, r.command)
                .await
                .unwrap();
            assert_eq!(result.event_head.is_some(), cancelled);
        }
    }
    assert_eq!(f.count("dispatch_messages").await, 4);
    f.store.integrity_check().await.unwrap();
}

struct InfrastructureFailure {
    inner: Arc<SyntheticHost>,
    target: Uuid,
}
#[async_trait]
impl HostGate for InfrastructureFailure {
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
        if context.context_id == self.target {
            return Err(Error::new(
                ErrorCode::StorageError,
                "Synthetic infrastructure failure.",
            ));
        }
        self.inner.verify(context).await
    }
}
#[tokio::test]
async fn infrastructure_failure_aborts_prior_successes_and_invalid_groups_never_reserve_writer() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let app = Application::new(
        f.store.clone(),
        f.clock.clone(),
        Arc::new(InfrastructureFailure {
            inner: f.host.clone(),
            target: b.context_id,
        }),
    );
    let requests = vec![
        request(&a, post("first provisional")),
        request(&b, post("second failure")),
    ];
    let before = f.counts().await;
    assert_eq!(
        app.execute_group(requests.clone()).await.unwrap_err().code,
        ErrorCode::StorageError
    );
    let mut tx = f.store.begin().await.unwrap();
    for r in &requests {
        assert!(tx.idempotency(r.request_id).await.unwrap().is_none());
    }
    tx.rollback().await.unwrap();
    assert_eq!(f.counts().await, before);
    let held = f.store.begin().await.unwrap();
    let result = tokio::time::timeout(Duration::from_millis(250), async {
        assert_eq!(
            f.app.execute_group(vec![]).await.unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            f.app
                .execute_group(vec![requests[0].clone(); MAX_MUTATION_GROUP + 1])
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        let bad = f
            .app
            .execute_group(vec![request(&a, post(" ")), request(&b, post("\t"))])
            .await
            .unwrap();
        assert_eq!(bad.len(), 2);
        assert!(
            bad.iter()
                .all(|r| r.as_ref().unwrap_err().code == ErrorCode::InvalidArgument)
        );
    })
    .await;
    held.rollback().await.unwrap();
    result.unwrap();
    for r in requests {
        assert!(
            f.app
                .execute(r.request_id, &r.context, r.command)
                .await
                .unwrap()
                .event_head
                .is_some()
        );
    }
    f.store.integrity_check().await.unwrap();
}
