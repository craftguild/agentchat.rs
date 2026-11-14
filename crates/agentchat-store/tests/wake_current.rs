//! Native file hints cannot outlive their persisted application authority.
mod support;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::{Application, Mutation},
    delivery::{HostReceipt, WakeDispatch},
    model::Phase,
    ports::{HostGate, HostState, Storage},
};
use async_trait::async_trait;
use std::sync::Arc;
use support::Fixture;
use uuid::Uuid;

struct RefuseHost(Arc<support::SyntheticHost>);
fn refs(dispatch: &WakeDispatch) -> Vec<Uuid> {
    dispatch
        .hint
        .events
        .iter()
        .map(|event| event.delivery_id)
        .collect()
}
#[async_trait]
impl HostGate for RefuseHost {
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
    async fn launch_identity(
        &self,
        context: &agentchat_core::model::Context,
    ) -> Result<Option<agentchat_core::model::LaunchIdentity>> {
        self.0.launch_identity(context).await
    }
    fn launch_owner(
        &self,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> agentchat_core::model::LaunchOwnerState {
        self.0.launch_owner(identity)
    }
    fn allows_unmanaged_participation(&self) -> bool {
        true
    }
    async fn verify(&self, _: &agentchat_core::model::Context) -> Result<HostState> {
        panic!("read-only claim validation must not invoke native getters");
    }
}

#[tokio::test]
async fn claim_check_is_read_only_and_rejects_foreign_retry_settlement_and_leave() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    f.join(&a, "alice").await;
    let bob = f.native_join(&b, "bob").await;
    f.native_join(&c, "cara").await;
    f.post(&a, "@bob exact pending dispatch").await;
    let claim = f.app.prepare_wake(&b).await.unwrap().unwrap();
    let app = Application::new(
        f.store.clone(),
        f.clock.clone(),
        Arc::new(RefuseHost(f.host.clone())),
    );
    let before = f.health().await;
    let mut s = f.store.snapshot().await.unwrap();
    let session = s.session(bob).await.unwrap();
    let batch = s.wake_batch(claim.hint.batch_id).await.unwrap();
    let head = s.head();
    drop(s);
    let writer = f.store.begin().await.unwrap();
    for _ in 0..3 {
        assert!(
            app.wake_is_current(&b, claim.hint.batch_id, claim.token, &refs(&claim))
                .await
                .unwrap()
        );
    }
    writer.rollback().await.unwrap();
    assert!(
        !app.wake_is_current(&c, claim.hint.batch_id, claim.token, &refs(&claim))
            .await
            .unwrap()
    );
    assert!(
        !app.wake_is_current(&b, claim.hint.batch_id, Uuid::new_v4(), &refs(&claim),)
            .await
            .unwrap()
    );
    assert!(
        !app.wake_is_current(&b, Uuid::new_v4(), claim.token, &refs(&claim))
            .await
            .unwrap()
    );
    assert_eq!(before, f.health().await);
    let mut s = f.store.snapshot().await.unwrap();
    assert_eq!(session, s.session(bob).await.unwrap());
    assert_eq!(batch, s.wake_batch(claim.hint.batch_id).await.unwrap());
    assert_eq!(head, s.head());
    drop(s);
    let mut changed = claim.clone();
    changed.hint.events.clear();
    assert!(
        !f.app
            .finish_wake(&b, &changed, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert!(
        app.wake_is_current(&b, claim.hint.batch_id, claim.token, &refs(&claim))
            .await
            .unwrap()
    );
    // A failed native attempt has no remaining prompt authority, including the
    // backoff interval before a replacement token is issued.
    assert!(
        f.app
            .finish_wake(
                &b,
                &claim,
                Err(Error::new(ErrorCode::IoError, "test failure"))
            )
            .await
            .unwrap()
    );
    assert!(
        !app.wake_is_current(&b, claim.hint.batch_id, claim.token, &refs(&claim))
            .await
            .unwrap()
    );
    f.clock.advance(1);
    let retry = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_ne!(retry.token, claim.token);
    assert!(
        !app.wake_is_current(&b, claim.hint.batch_id, claim.token, &refs(&claim))
            .await
            .unwrap()
    );
    assert!(
        app.wake_is_current(&b, retry.hint.batch_id, retry.token, &refs(&retry))
            .await
            .unwrap()
    );
    f.app
        .finish_wake(&b, &retry, Ok(HostReceipt { turn_id: None }))
        .await
        .unwrap();
    assert!(
        app.wake_is_current(&b, retry.hint.batch_id, retry.token, &refs(&retry))
            .await
            .unwrap()
    );
    f.accept_native_inputs(
        &b,
        &(retry
            .hint
            .events
            .iter()
            .map(|r| r.delivery_id)
            .collect::<Vec<_>>()),
    )
    .await;
    assert!(
        !app.wake_is_current(&b, retry.hint.batch_id, retry.token, &refs(&retry))
            .await
            .unwrap()
    );
    f.post(&a, "@bob then leave").await;
    let leaving = f.app.prepare_wake(&b).await.unwrap().unwrap();
    f.run(&b, Mutation::Leave).await;
    assert!(
        !app.wake_is_current(&b, leaving.hint.batch_id, leaving.token, &refs(&leaving),)
            .await
            .unwrap()
    );
    assert_eq!(f.count("read_receipts").await, 0);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn expired_detached_reconnected_and_paused_claims_cannot_be_accepted() {
    for case in [
        "expiry",
        "detach",
        "reconnect",
        "pause",
        "approval",
        "heartbeat_expiry",
    ] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        f.native_join(&b, "bob").await;
        f.post(&a, "@bob pending").await;
        let claim = f.app.prepare_wake(&b).await.unwrap().unwrap();
        match case {
            "expiry" => f.clock.advance(5),
            "heartbeat_expiry" => {
                f.app
                    .finish_wake(&b, &claim, Ok(HostReceipt { turn_id: None }))
                    .await
                    .unwrap();
                f.clock.advance(45);
            }
            "detach" => f.app.detach(&b).await.unwrap(),
            "reconnect" => {
                let mut next = b.clone();
                next.incarnation = Uuid::new_v4();
                next.context_id = Uuid::new_v4();
                f.native_reconnect(&b, &next);
                f.app.attach(&next).await.unwrap();
            }
            "pause" | "approval" => {
                f.host.phase(
                    &b,
                    if case == "pause" {
                        Phase::Paused
                    } else {
                        Phase::ApprovalBlocked
                    },
                );
                f.app.heartbeat(&b).await.unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            !f.app
                .wake_is_current(&b, claim.hint.batch_id, claim.token, &refs(&claim))
                .await
                .unwrap(),
            "{case}"
        );
        assert_eq!(f.count("read_receipts").await, 0);
    }
}

#[tokio::test]
async fn processing_deadline_stops_pending_and_inflight_hints_without_housekeeping() {
    for inflight in [false, true] {
        let f = Fixture::new().await;
        f.timeout(1).await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        f.native_join(&b, "bob").await;
        f.post(&a, "@bob original deadline").await;
        let original = f.app.prepare_wake(&b).await.unwrap().unwrap();
        if inflight {
            f.app
                .finish_wake(&b, &original, Ok(HostReceipt::default()))
                .await
                .unwrap();
        }
        let ids = refs(&original);
        f.clock.advance(1);
        assert!(
            f.app
                .wake_is_current(&b, original.hint.batch_id, original.token, &ids)
                .await
                .unwrap()
        );
        let before = f.health().await;
        let mut snapshot = f.store.snapshot().await.unwrap();
        let batch = snapshot.wake_batch(original.hint.batch_id).await.unwrap();
        drop(snapshot);
        f.clock.advance(1);
        assert!(
            !f.app
                .wake_is_current(&b, original.hint.batch_id, original.token, &ids)
                .await
                .unwrap()
        );
        let mut snapshot = f.store.snapshot().await.unwrap();
        assert_eq!(
            batch,
            snapshot.wake_batch(original.hint.batch_id).await.unwrap()
        );
        drop(snapshot);
        assert_eq!(before, f.health().await);
        // The next ordinary preparation observes expiry without issuing a new
        // hint; full-input uncertainty remains governed by its original stream.
        f.clock.advance(3);
        assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
        assert_eq!(f.count("delivery_timeout_notices").await, 1);
        assert_eq!(f.count("read_receipts").await, 0);
    }
}
