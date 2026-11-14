//! Preparation groups keep ordinary history, original claims and FULL boundaries.
mod support;
use agentchat_core::{
    application::{CommitOutcome, CommitRequest, NativePreparation},
    delivery::{HostReceipt, MessageProgress, NativeCompletion},
    model::DeliveryState,
    ports::Storage,
};
use support::{Fixture, native::Gate};

async fn processing(
    app: &agentchat_core::application::Application,
    contexts: &[agentchat_core::model::Context],
) -> Vec<agentchat_core::application::ProcessingReady> {
    app.commit_ready(
        contexts
            .iter()
            .map(|c| CommitRequest::NativeProcessing(app.native_processing(c).unwrap()))
            .collect(),
    )
    .await
    .unwrap()
    .into_iter()
    .map(|r| match r.unwrap() {
        CommitOutcome::NativeProcessing(p) => p,
        _ => panic!("processing outcome"),
    })
    .collect()
}
#[tokio::test]
async fn independent_preparations_share_one_group_and_original_results_settle_once() {
    let f = Fixture::new().await;
    let author = f.context();
    f.join(&author, "author").await;
    let gate = Gate::new(&f);
    let app = gate.app(&f);
    let recipients = [f.context(), f.context()];
    for (i, c) in recipients.iter().enumerate() {
        gate.bind(c);
        app.start_launch(c, Some(&format!("receiver{i}")))
            .await
            .unwrap();
    }
    // Consume ordinary startup notices through the same unchanged singleton API.
    for c in &recipients {
        while let Some(d) = app.prepare_native(c).await.unwrap() {
            assert!(
                app.finish_native(c, &d, Ok(MessageProgress::Accepted(HostReceipt::default())))
                    .await
                    .unwrap()
            );
        }
    }
    f.post(&author, "@receiver0 @receiver1 exact input").await;
    let prepared = processing(&app, &recipients).await;
    // Host observation cannot start until committed Processing/history is visible.
    let mut tx = f.store.begin().await.unwrap();
    for c in &recipients {
        let agent = tx
            .conversation(c.host, &c.host_session_id)
            .await
            .unwrap()
            .unwrap();
        let launch = tx.latest_launch(agent.id).await.unwrap().unwrap();
        let stream = tx
            .message_stream(launch.identity.generation)
            .await
            .unwrap()
            .unwrap();
        assert!(stream.head.is_none());
        let item = tx
            .next_native(agent.id, stream.settled_seq)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.delivery().state, DeliveryState::Processing);
        assert!(item.delivery().history.processing_started_at.is_some());
    }
    tx.rollback().await.unwrap();
    let mut claims = Vec::new();
    for p in prepared {
        match app.observe_native(p).await.unwrap() {
            NativePreparation::Claim(c) => claims.push(CommitRequest::NativeClaim(c)),
            _ => panic!("fresh claim"),
        }
    }
    let outcomes = app.commit_ready(claims).await.unwrap();
    let dispatches = outcomes
        .into_iter()
        .map(|v| match v.unwrap() {
            CommitOutcome::NativeClaim(c) => c.into_dispatch().unwrap(),
            _ => panic!("claim outcome"),
        })
        .collect::<Vec<_>>();
    assert_eq!(dispatches.len(), 2);
    let results = app
        .commit_ready(
            recipients
                .iter()
                .zip(&dispatches)
                .map(|(c, d)| {
                    CommitRequest::NativeCompletion(NativeCompletion {
                        context: c.clone(),
                        dispatch: d.clone(),
                        outcome: Ok(MessageProgress::Accepted(HostReceipt::default())),
                    })
                })
                .collect(),
        )
        .await
        .unwrap();
    assert!(
        results
            .iter()
            .all(|r| matches!(r, Ok(CommitOutcome::NativeCompletion(true))))
    );
    for (c, d) in recipients.iter().zip(&dispatches) {
        assert!(
            !app.finish_native(c, d, Ok(MessageProgress::Accepted(HostReceipt::default())))
                .await
                .unwrap()
        );
        assert!(app.prepare_native(c).await.unwrap().is_none());
    }
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn observed_claim_rechecks_departure_and_does_not_resurrect_an_original_head() {
    let f = Fixture::new().await;
    let author = f.context();
    f.join(&author, "author").await;
    let gate = Gate::new(&f);
    let c = f.context();
    gate.bind(&c);
    let app = gate.app(&f);
    app.start_launch(&c, Some("receiver")).await.unwrap();
    f.post(&author, "@receiver original").await;
    let p = processing(&app, std::slice::from_ref(&c))
        .await
        .pop()
        .unwrap();
    let NativePreparation::Claim(claim) = app.observe_native(p).await.unwrap() else {
        panic!("fresh claim")
    };
    app.execute(
        uuid::Uuid::new_v4(),
        &c,
        agentchat_core::application::Mutation::Leave,
    )
    .await
    .unwrap();
    let outcome = app
        .commit_ready(vec![CommitRequest::NativeClaim(claim)])
        .await
        .unwrap()
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.native_preparation_changed(), Some(false));
    let CommitOutcome::NativeClaim(value) = outcome else {
        panic!("claim")
    };
    assert!(value.into_dispatch().is_none());
    let outcome = app
        .commit_ready(vec![CommitRequest::NativeProcessing(
            app.native_processing(&c).unwrap(),
        )])
        .await
        .unwrap()
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.native_preparation_changed(), Some(false));
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn caller_cancelled_while_waiting_for_begin_never_mutates_but_independent_ready_work_commits()
{
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    let f = Fixture::new().await;
    let author = f.context();
    f.join(&author, "author").await;
    let gate = Gate::new(&f);
    let app = Arc::new(gate.app(&f));
    let contexts = [f.context(), f.context()];
    for (i, c) in contexts.iter().enumerate() {
        gate.bind(c);
        app.start_launch(c, Some(&format!("receiver{i}")))
            .await
            .unwrap();
    }
    for c in &contexts {
        while let Some(d) = app.prepare_native(c).await.unwrap() {
            app.finish_native(c, &d, Ok(MessageProgress::Accepted(HostReceipt::default())))
                .await
                .unwrap();
        }
    }
    f.post(&author, "@all pending input").await;
    let held = f.store.begin().await.unwrap();
    let (cancelled, ticket) = app
        .native_processing(&contexts[0])
        .unwrap()
        .until(Instant::now() + Duration::from_secs(5));
    let independent = app.native_processing(&contexts[1]).unwrap();
    let task = tokio::spawn({
        let app = app.clone();
        async move {
            app.commit_ready(vec![
                CommitRequest::NativeProcessing(cancelled),
                CommitRequest::NativeProcessing(independent),
            ])
            .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    drop(ticket);
    held.rollback().await.unwrap();
    let outcomes = task.await.unwrap().unwrap();
    assert_eq!(
        outcomes[0].as_ref().unwrap_err().code,
        agentchat_core::ErrorCode::RuntimeUnavailable
    );
    assert_eq!(
        outcomes[1].as_ref().unwrap().native_preparation_changed(),
        Some(true)
    );
    let mut tx = f.store.begin().await.unwrap();
    for (i, c) in contexts.iter().enumerate() {
        let a = tx
            .conversation(c.host, &c.host_session_id)
            .await
            .unwrap()
            .unwrap();
        let generation = tx
            .latest_launch(a.id)
            .await
            .unwrap()
            .unwrap()
            .identity
            .generation;
        let stream = tx.message_stream(generation).await.unwrap().unwrap();
        assert!(stream.head.is_none());
        let item = tx
            .next_native(a.id, stream.settled_seq)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            item.delivery().state,
            if i == 0 {
                DeliveryState::Pending
            } else {
                DeliveryState::Processing
            }
        );
    }
    tx.rollback().await.unwrap();
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn cancelled_observed_claim_waiting_for_begin_keeps_processing_without_admission() {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let gate = Gate::new(&f);
    let b = f.context();
    gate.bind(&b);
    let app = Arc::new(gate.app(&f));
    app.start_launch(&b, Some("recipient")).await.unwrap();
    while let Some(d) = app.prepare_native(&b).await.unwrap() {
        app.finish_native(
            &b,
            &d,
            Ok(MessageProgress::Accepted(HostReceipt::default())),
        )
        .await
        .unwrap();
    }
    f.post(&a, "@recipient original").await;
    let (request, ticket) = app
        .native_processing(&b)
        .unwrap()
        .until(Instant::now() + Duration::from_secs(5));
    let outcome = app
        .commit_ready(vec![CommitRequest::NativeProcessing(request)])
        .await
        .unwrap()
        .pop()
        .unwrap()
        .unwrap();
    let CommitOutcome::NativeProcessing(p) = outcome else {
        panic!("processing")
    };
    let NativePreparation::Claim(claim) = app.observe_native(p).await.unwrap() else {
        panic!("claim")
    };
    let held = f.store.begin().await.unwrap();
    let task = tokio::spawn({
        let app = app.clone();
        async move {
            app.commit_ready(vec![CommitRequest::NativeClaim(claim)])
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    drop(ticket);
    held.rollback().await.unwrap();
    assert_eq!(
        task.await
            .unwrap()
            .unwrap()
            .pop()
            .unwrap()
            .unwrap_err()
            .code,
        agentchat_core::ErrorCode::RuntimeUnavailable
    );
    let mut tx = f.store.begin().await.unwrap();
    let agent = tx
        .conversation(b.host, &b.host_session_id)
        .await
        .unwrap()
        .unwrap();
    let generation = tx
        .latest_launch(agent.id)
        .await
        .unwrap()
        .unwrap()
        .identity
        .generation;
    let stream = tx.message_stream(generation).await.unwrap().unwrap();
    assert!(stream.head.is_none());
    let item = tx
        .next_native(agent.id, stream.settled_seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.delivery().state, DeliveryState::Processing);
    assert_eq!(item.delivery().attempts, 0);
    tx.rollback().await.unwrap();
    // Ordinary fresh preparation still works, using its first original token.
    let dispatch = app.prepare_native(&b).await.unwrap().unwrap();
    assert_eq!(dispatch.attempt(), 1);
    assert!(
        app.finish_native(
            &b,
            &dispatch,
            Ok(MessageProgress::Accepted(HostReceipt::default()))
        )
        .await
        .unwrap()
    );
    f.store.integrity_check().await.unwrap();
}

struct ObservedHost {
    inner: std::sync::Arc<support::SyntheticHost>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl agentchat_core::ports::HostGate for ObservedHost {
    fn input_capabilities(
        &self,
        c: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        self.inner.input_capabilities(c)
    }
    fn allows_unmanaged_participation(&self) -> bool {
        self.inner.allows_unmanaged_participation()
    }
    async fn launch_identity(
        &self,
        c: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<Option<agentchat_core::model::LaunchIdentity>> {
        self.inner.launch_identity(c).await
    }
    fn launch_owner(
        &self,
        i: &agentchat_core::model::LaunchIdentity,
    ) -> agentchat_core::model::LaunchOwnerState {
        self.inner.launch_owner(i)
    }
    async fn verify(
        &self,
        c: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::ports::HostState> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.verify(c).await
    }
}
#[tokio::test]
async fn processing_selection_cannot_lend_host_observation_to_a_successor_or_new_arrival() {
    use std::{sync::Arc, time::Duration};
    for initially_empty in [false, true] {
        let f = Fixture::new().await;
        let a = f.context();
        f.join(&a, "author").await;
        let b = f.context();
        f.native_join(&b, "recipient").await;
        while let Some(d) = f.app.prepare_native(&b).await.unwrap() {
            f.app
                .finish_native(
                    &b,
                    &d,
                    Ok(MessageProgress::Accepted(HostReceipt::default())),
                )
                .await
                .unwrap();
        }
        if !initially_empty {
            f.post(&a, "@recipient first original").await;
            f.post(&a, "@recipient successor").await;
        }
        let host = Arc::new(ObservedHost {
            inner: f.host.clone(),
            entered: Default::default(),
            release: Default::default(),
        });
        let app = Arc::new(agentchat_core::application::Application::new(
            f.store.clone(),
            f.clock.clone(),
            host.clone(),
        ));
        let p = processing(&app, std::slice::from_ref(&b))
            .await
            .pop()
            .unwrap();
        let observation = tokio::spawn({
            let app = app.clone();
            async move { app.observe_native(p).await }
        });
        tokio::time::timeout(Duration::from_secs(2), host.entered.notified())
            .await
            .unwrap();
        let original = if initially_empty {
            f.post(&a, "@recipient newly arrived").await;
            None
        } else {
            let d = f.app.prepare_native(&b).await.unwrap().unwrap();
            assert!(
                f.app
                    .finish_native(
                        &b,
                        &d,
                        Ok(MessageProgress::Accepted(HostReceipt::default()))
                    )
                    .await
                    .unwrap()
            );
            Some(d)
        };
        host.release.notify_one();
        let NativePreparation::Claim(claim) = observation.await.unwrap().unwrap() else {
            panic!("observed original P")
        };
        let result = app
            .commit_ready(vec![CommitRequest::NativeClaim(claim)])
            .await
            .unwrap()
            .pop()
            .unwrap()
            .unwrap();
        assert_eq!(result.native_preparation_changed(), Some(false));
        let CommitOutcome::NativeClaim(value) = result else {
            panic!("claim")
        };
        assert!(value.into_dispatch().is_none());
        // A new complete P/observe/H, rather than reusing the old captured state.
        let p = processing(&app, std::slice::from_ref(&b))
            .await
            .pop()
            .unwrap();
        host.release.notify_one();
        let NativePreparation::Claim(claim) = app.observe_native(p).await.unwrap() else {
            panic!("new P")
        };
        let CommitOutcome::NativeClaim(value) = app
            .commit_ready(vec![CommitRequest::NativeClaim(claim)])
            .await
            .unwrap()
            .pop()
            .unwrap()
            .unwrap()
        else {
            panic!("fresh H")
        };
        let fresh = value.into_dispatch().unwrap();
        if let Some(original) = original {
            assert!(fresh.seq() > original.seq());
            assert_ne!(fresh.delivery_id(), original.delivery_id());
        }
        assert!(
            app.finish_native(
                &b,
                &fresh,
                Ok(MessageProgress::Accepted(HostReceipt::default()))
            )
            .await
            .unwrap()
        );
        f.store.integrity_check().await.unwrap();
    }
}
