//! Exact original jobs retry once only after newer committed work.
use crate::daemon::delivery::{Dispatches, Finished, Outcome};
use agentchat_core::{application::CommitOutcome, model::DeliveryState, ports::Storage};
use std::{sync::atomic::Ordering, time::Duration};
use uuid::Uuid;
#[path = "native_rearm_fixture.rs"]
mod fixture;
use fixture::Fixture;
async fn outcome(jobs: &mut Dispatches) -> Outcome {
    tokio::time::timeout(Duration::from_secs(3), jobs.receiver.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn apply(f: &Fixture, finished: Vec<Finished>) {
    let results = f
        .app
        .commit_ready(finished.into_iter().map(Finished::request).collect())
        .await
        .unwrap();
    for result in results {
        assert!(matches!(
            result.unwrap(),
            CommitOutcome::NativeCompletion(_)
        ));
    }
}
#[tokio::test]
async fn actual_empty_job_rearms_later_durable_work_once_and_settles_original_receipt() {
    let mut f = Fixture::new(2, 1).await;
    let mut worker = f.worker().await;
    let recipient = f.contexts[1].clone();
    let head0 = f.head().await;
    worker
        .deliveries
        .start_at(&f.app, vec![recipient.clone()], head0)
        .await
        .unwrap();
    f.barrier(1).await;
    let original = worker.deliveries.test_job(recipient.context_id).unwrap();
    f.post().await;
    let head1 = f.head().await;
    assert!(head1 > head0);
    assert!(f.next(1).await.is_some());
    // A notification while the original empty preparation is still active
    // cannot start a replacement job or present the later durable work.
    worker
        .deliveries
        .start_at(&f.app, vec![recipient.clone()], head1)
        .await
        .unwrap();
    assert_eq!(
        worker.deliveries.test_job(recipient.context_id).unwrap(),
        original
    );
    assert_eq!(f.gate.calls.load(Ordering::SeqCst), 1);
    f.gate.resume.add_permits(1);
    let result = outcome(&mut worker.deliveries).await;
    let finished = worker.take_native_ready(Some(result), 8);
    assert!(finished.is_empty());
    assert_eq!(worker.native_empty_at, Some(head0));
    assert_eq!(f.port.fresh.load(Ordering::SeqCst), 0);
    // Exercise the real coordinator tail, including durable-head publication.
    // No housekeeping tick, extra business write or native completion exists.
    assert!(worker.commit_group(vec![], finished, true).await);
    assert_eq!(*worker.heads.borrow(), head1);
    assert_eq!(worker.native_empty_at, None);
    assert!(!worker.commit_group(vec![], vec![], true).await);
    assert!(worker.last_error.is_none());
    worker
        .deliveries
        .start_at(&f.app, vec![recipient], *worker.heads.borrow())
        .await
        .unwrap();
    let result = outcome(&mut worker.deliveries).await;
    let finished = worker.take_native_ready(Some(result), 8);
    assert_eq!(finished.len(), 1);
    let Finished::Native(ref completion) = finished[0] else {
        panic!("native completion")
    };
    let delivery = completion.dispatch.delivery_id();
    let dispatch = completion.dispatch.clone();
    assert!(worker.commit_group(vec![], finished, true).await);
    let mut tx = f.store.begin().await.unwrap();
    let row = tx.delivery(delivery).await.unwrap().unwrap();
    assert_eq!(row.state, DeliveryState::Delivered);
    assert_eq!(row.attempts, 1);
    tx.rollback().await.unwrap();
    assert!(
        !f.app
            .finish_native(
                &f.contexts[1],
                &dispatch,
                Ok(agentchat_core::delivery::MessageProgress::Accepted(
                    Default::default()
                ))
            )
            .await
            .unwrap()
    );
    assert_eq!(f.gate.calls.load(Ordering::SeqCst), 2);
    assert_eq!(f.port.fresh.load(Ordering::SeqCst), 1);
    assert!(f.next(1).await.is_none());
    assert_eq!(worker.deliveries.status().jobs, 0);
    f.store.integrity_check().await.unwrap();
    f.log.close().await.unwrap();
}
#[tokio::test]
async fn five_same_head_empty_jobs_never_self_rearm_or_ping_pong() {
    let mut f = Fixture::new(5, 5).await;
    let mut jobs = f.dispatches();
    let head = f.head().await;
    jobs.start(&f.app, f.contexts.clone()).await.unwrap();
    f.barrier(5).await;
    jobs.start_at(&f.app, f.contexts.clone(), head)
        .await
        .unwrap();
    assert_eq!(f.gate.calls.load(Ordering::SeqCst), 5);
    f.gate.resume.add_permits(5);
    let mut received = 0;
    while received < 5 {
        let result = outcome(&mut jobs).await;
        let (finished, failure, completed, empty_at) = jobs.take_ready_with_empty(Some(result), 1);
        assert!(finished.is_empty() && failure.is_none() && completed);
        assert_eq!(empty_at, Some(head));
        assert_eq!(f.head().await, head);
        received += 1;
    }
    assert!(jobs.status().jobs == 0);
    assert!(jobs.receiver.is_empty());
    assert_eq!(f.gate.calls.load(Ordering::SeqCst), 5);
    assert_eq!(f.port.fresh.load(Ordering::SeqCst), 0);
    assert_eq!(f.port.polls.load(Ordering::SeqCst), 0);
    f.log.close().await.unwrap();
}
#[tokio::test]
async fn stale_and_duplicate_empty_results_never_rearm_a_replacement_but_positive_facts_survive() {
    let mut f = Fixture::new(2, 2).await;
    let mut jobs = f.dispatches();
    let recipient = f.contexts[1].clone();
    let head = f.head().await;
    jobs.start_at(&f.app, vec![recipient.clone()], head)
        .await
        .unwrap();
    f.barrier(1).await;
    let old = jobs.test_job(recipient.context_id).unwrap();
    jobs.cancel(recipient.context_id);
    jobs.start_at(&f.app, vec![recipient.clone()], head)
        .await
        .unwrap();
    f.barrier(1).await;
    let replacement = jobs.test_job(recipient.context_id).unwrap();
    assert_ne!(old, replacement);
    for (job, context) in [(old, recipient.context_id), (replacement, Uuid::new_v4())] {
        let result = Outcome::test_empty(job, context);
        let (finished, failure, _, empty) = jobs.take_ready_with_empty(Some(result), 8);
        assert!(finished.is_empty() && failure.is_none());
        assert_eq!(empty, None);
        assert_eq!(jobs.test_job(recipient.context_id).unwrap(), replacement);
    }
    f.gate.resume.add_permits(1);
    let result = outcome(&mut jobs).await;
    let (_, _, _, empty) = jobs.take_ready_with_empty(Some(result), 8);
    assert_eq!(empty, Some(head));
    let result = Outcome::test_empty(replacement, recipient.context_id);
    assert_eq!(jobs.take_ready_with_empty(Some(result), 8).3, None);
    f.post().await;
    let head = f.head().await;
    jobs.start_at(&f.app, vec![recipient], head).await.unwrap();
    let result = outcome(&mut jobs).await.test_with_job(old);
    let (finished, failure, _) = jobs.take_ready(Some(result), 8);
    assert!(failure.is_none());
    assert_eq!(finished.len(), 1);
    apply(&f, finished).await;
    assert!(f.next(1).await.is_none());
    f.log.close().await.unwrap();
}
#[tokio::test]
async fn a_new_work_revision_never_resubmits_an_original_unknown_head() {
    let f = Fixture::new(2, 0).await;
    let mut jobs = f.dispatches();
    let recipient = f.contexts[1].clone();
    f.port.uncertain.store(true, Ordering::SeqCst);
    f.post().await;
    jobs.start_at(&f.app, vec![recipient.clone()], f.head().await)
        .await
        .unwrap();
    let result = outcome(&mut jobs).await;
    let (finished, failure, _, _) = jobs.take_ready_with_empty(Some(result), 8);
    assert!(failure.is_none());
    let Finished::Native(ref original) = finished[0] else {
        panic!("native")
    };
    let dispatch = original.dispatch.clone();
    let id = dispatch.delivery_id();
    let token = dispatch.token();
    let generation = dispatch.generation();
    apply(&f, finished).await;
    let mut tx = f.store.begin().await.unwrap();
    let row = tx.delivery(id).await.unwrap().unwrap();
    assert_eq!(row.state, DeliveryState::Processing);
    assert_eq!(row.attempts, 1);
    tx.rollback().await.unwrap();
    let original_stream = f.stream(1).await;
    let original_head = original_stream.head.as_ref().unwrap();
    assert_eq!(original_head.token, token);
    assert_eq!(original_head.attempts, 1);
    assert!(original_head.submission_pending);
    assert!(original_head.failure_code.is_some());
    f.post().await;
    let head = f.head().await;
    jobs.start_at(&f.app, vec![recipient], head).await.unwrap();
    let result = outcome(&mut jobs).await;
    let (finished, failure, _, empty) = jobs.take_ready_with_empty(Some(result), 8);
    assert!(failure.is_none());
    assert!(empty.is_none_or(|at| at == head));
    for value in &finished {
        let Finished::Native(c) = value else {
            panic!("native")
        };
        assert_eq!(c.dispatch.delivery_id(), id);
        assert_eq!(c.dispatch.token(), token);
        assert_eq!(c.dispatch.generation(), generation);
        assert!(c.dispatch.awaiting_acceptance());
    }
    if !finished.is_empty() {
        apply(&f, finished).await;
    }
    let mut tx = f.store.begin().await.unwrap();
    let row = tx.delivery(id).await.unwrap().unwrap();
    assert_eq!(row.state, DeliveryState::Processing);
    assert_eq!(row.attempts, 1);
    tx.rollback().await.unwrap();
    assert_eq!(f.port.fresh.load(Ordering::SeqCst), 1);
    assert_eq!(f.port.polls.load(Ordering::SeqCst), 0);
    assert_eq!(f.stream(1).await, original_stream);
    f.store.integrity_check().await.unwrap();
    f.log.close().await.unwrap();
}
