//! Conservative hints must neither hide required reconciliation nor grant wake.
mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    delivery::{BatchState, MAX_WAKE_SCAN_CONTEXTS},
    model::{
        Delivery, DeliveryState, DispatchKind, DispatchNotice, DispatchPayload, HostKind,
        NoticeKind, Phase,
    },
    ports::Storage,
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query},
};
use std::{collections::BTreeSet, time::Duration};
use support::Fixture;
use uuid::Uuid;

#[tokio::test]
async fn exact_host_session_pairs_ignore_passive_history_and_do_not_inherit_directory_identity() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let mut c = b.clone();
    c.host = HostKind::Claude;
    c.context_id = Uuid::new_v4();
    c.incarnation = Uuid::new_v4();
    let fork = f.context();
    f.join(&a, "alice").await;
    f.native_join(&b, "bob").await;
    f.native_join(&c, "cara").await;
    f.post(&a, "passive context").await;
    let mut contexts = vec![a, b, c.clone(), fork];
    assert!(f.app.wake_candidates(&contexts).await.unwrap().is_empty());
    f.post(&contexts[0], "@cara exact frozen recipient").await;
    let mut alternate = c.clone();
    alternate.context_id = Uuid::new_v4();
    alternate.incarnation = Uuid::new_v4();
    contexts.push(alternate.clone());
    assert_eq!(
        f.app.wake_candidates(&contexts).await.unwrap(),
        BTreeSet::from([c.context_id, alternate.context_id])
    );
    // A conservative positive for the same conversation is not permission to
    // use an unbound descriptor or replacement incarnation.
    assert!(!matches!(f.app.prepare_wake(&alternate).await, Ok(Some(_))));
    assert_eq!(f.count("wake_batches").await, 0);
    f.run(&c, Mutation::Leave).await;
    assert!(f.app.wake_candidates(&contexts).await.unwrap().is_empty());
    assert!(
        f.store
            .wake_candidates(&[], f.clock.now())
            .await
            .unwrap()
            .is_empty()
    );
    let bounded = vec![contexts[0].clone(); MAX_WAKE_SCAN_CONTEXTS];
    assert!(f.app.wake_candidates(&bounded).await.unwrap().is_empty());
    let oversized = vec![contexts[0].clone(); MAX_WAKE_SCAN_CONTEXTS + 1];
    assert_eq!(
        f.app.wake_candidates(&oversized).await.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        f.store
            .wake_candidates(&oversized, f.clock.now())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn read_hint_does_not_wait_for_a_writer_or_cache_uncommitted_publication() {
    let f = Fixture::new().await;
    let context = f.context();
    let recipient = f.join(&context, "recipient").await;
    for commit in [false, true] {
        let mut tx = f.store.begin().await.unwrap();
        let header = tx
            .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
            .await
            .unwrap();
        tx.publish_dispatch(
            &header,
            &DispatchPayload::Notice(DispatchNotice {
                dispatch_id: header.id,
                kind: NoticeKind::AgentJoined,
                data: serde_json::json!({"agent_id":recipient,"handle":"recipient"}),
            }),
            &[Delivery {
                id: Uuid::new_v4(),
                dispatch_id: header.id,
                recipient_id: recipient,
                state: DeliveryState::Pending,
                wake_eligible: true,
                wake_batch_id: None,
                attempts: 0,
                history: agentchat_core::model::DeliveryHistory::queued(f.clock.now()),
            }],
        )
        .await
        .unwrap();
        let candidates = tokio::time::timeout(
            Duration::from_secs(1),
            f.app.wake_candidates(std::slice::from_ref(&context)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            candidates.is_empty(),
            "uncommitted work must remain invisible"
        );
        if commit {
            tx.commit().await.unwrap();
        } else {
            tx.rollback().await.unwrap();
        }
        assert_eq!(
            f.app
                .wake_candidates(std::slice::from_ref(&context))
                .await
                .unwrap()
                .contains(&context.context_id),
            commit,
            "every call must use a fresh read"
        );
    }
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn complete_native_acceptance_settles_all_batch_states_without_model_ack() {
    for state in [
        BatchState::Pending,
        BatchState::Inflight,
        BatchState::Stalled,
    ] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        f.native_join(&b, "bob").await;
        f.post(&a, "@bob reconcile even during backoff or stall")
            .await;
        let claim = f.app.prepare_wake(&b).await.unwrap().unwrap();
        let mut tx = f.store.begin().await.unwrap();
        let mut batch = tx.wake_batch(claim.hint.batch_id).await.unwrap().unwrap();
        batch.state = state;
        tx.save_wake_batch(&batch).await.unwrap();
        tx.commit().await.unwrap();
        f.accept_native_inputs(
            &b,
            &claim
                .hint
                .events
                .iter()
                .map(|event| event.delivery_id)
                .collect::<Vec<_>>(),
        )
        .await;
        assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
        assert!(
            f.app
                .wake_candidates(std::slice::from_ref(&b))
                .await
                .unwrap()
                .is_empty()
        );
        let mut tx = f.store.begin().await.unwrap();
        assert_eq!(
            tx.wake_batch(batch.id).await.unwrap().unwrap().state,
            BatchState::Settled
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn positive_hints_never_refresh_presence_or_override_host_controls() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.native_join(&b, "bob").await;
    f.post(&a, "@bob retained work").await;
    let counts = f.counts().await;
    let mut tx = f.store.begin().await.unwrap();
    let before = tx.session(bob).await.unwrap().unwrap();
    tx.rollback().await.unwrap();
    for phase in [Phase::Paused, Phase::ApprovalBlocked, Phase::Disconnected] {
        f.host.phase(&b, phase);
        assert!(
            f.app
                .wake_candidates(std::slice::from_ref(&b))
                .await
                .unwrap()
                .contains(&b.context_id)
        );
        assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    }
    f.host.phase(&b, Phase::Idle);
    f.clock.advance(45);
    assert!(
        f.app
            .wake_candidates(std::slice::from_ref(&b))
            .await
            .unwrap()
            .contains(&b.context_id)
    );
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.session(bob).await.unwrap().unwrap(), before);
    tx.rollback().await.unwrap();
    assert_eq!(f.counts().await, counts);
}

#[tokio::test]
async fn matching_malformed_work_is_retained_for_ordinary_integrity_validation() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.native_join(&b, "bob").await;
    let message = f
        .post(&a, "@bob validate full rows after candidate selection")
        .await;
    let delivery = f
        .delivery(support::id(&message.result, "message_id"), bob)
        .await;
    let initial = f
        .rows("delivery_state_transitions")
        .await
        .into_iter()
        .find(|r| r.try_get::<String>("", "delivery_id").unwrap() == delivery.to_string())
        .unwrap()
        .try_get::<String>("", "id")
        .unwrap();
    f.inspect
        .execute(
            &Query::update()
                .table(Alias::new("delivery_state_transitions"))
                .value(Alias::new("id"), "malformed-id")
                .and_where(Expr::col(Alias::new("id")).eq(&initial))
                .to_owned(),
        )
        .await
        .unwrap();
    assert!(
        f.app
            .wake_candidates(std::slice::from_ref(&b))
            .await
            .unwrap()
            .contains(&b.context_id)
    );
    assert_eq!(
        f.app.prepare_wake(&b).await.unwrap_err().code,
        ErrorCode::IntegrityError
    );
    assert_eq!(f.count("wake_batches").await, 0);
    f.inspect
        .execute(
            &Query::update()
                .table(Alias::new("delivery_state_transitions"))
                .value(Alias::new("id"), initial.clone())
                .and_where(Expr::col(Alias::new("id")).eq("malformed-id"))
                .to_owned(),
        )
        .await
        .unwrap();
    for sealed in [false, true] {
        f.inspect
            .execute(
                &Query::update()
                    .table(Alias::new("dispatches"))
                    .value(Alias::new("sealed"), sealed)
                    .and_where(
                        Expr::col(Alias::new("id"))
                            .eq(message.result["message_id"].as_str().unwrap()),
                    )
                    .to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(
            f.app
                .wake_candidates(std::slice::from_ref(&b))
                .await
                .unwrap()
                .contains(&b.context_id),
            sealed
        );
    }
    assert!(f.app.prepare_wake(&b).await.unwrap().is_some());
    f.store.integrity_check().await.unwrap();
}
