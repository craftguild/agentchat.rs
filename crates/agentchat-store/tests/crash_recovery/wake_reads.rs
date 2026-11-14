//! Count actual storage-port work while exercising ordinary SQLite wake policy.
use super::*;
use agentchat_core::delivery::Presentation;

pub(super) enum Read {
    Batch,
    Pending,
    Items,
}
static BATCH: AtomicUsize = AtomicUsize::new(0);
static PENDING: AtomicUsize = AtomicUsize::new(0);
static ITEMS: AtomicUsize = AtomicUsize::new(0);
pub(super) fn record(point: &str, kind: Read) {
    if point != "wake_read_probe" {
        return;
    }
    let counter = match kind {
        Read::Batch => &BATCH,
        Read::Pending => &PENDING,
        Read::Items => &ITEMS,
    };
    counter.fetch_add(1, Ordering::SeqCst);
}
fn reads(batch: usize, pending: usize, items: usize) {
    assert_eq!(
        [
            BATCH.swap(0, Ordering::SeqCst),
            PENDING.swap(0, Ordering::SeqCst),
            ITEMS.swap(0, Ordering::SeqCst),
        ],
        [batch, pending, items],
        "batch lookup / bounded delivery probe / full event hydration"
    );
}
async fn assert_settled(f: &Fixture, batch: Uuid) {
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.wake_batch(batch).await.unwrap().unwrap().state,
        BatchState::Settled,
        "complete-input acceptance must settle its empty hint set atomically"
    );
    tx.rollback().await.unwrap();
}

fn failure() -> Result<HostReceipt> {
    Err(agentchat_core::Error::new(
        agentchat_core::ErrorCode::IoError,
        "Synthetic native failure.",
    ))
}
#[tokio::test]
async fn backoff_active_turn_stall_and_settlement_use_bounded_checks_before_event_hydration() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let gate = support::native::Gate::new(&f);
    gate.bind(&b);
    let native = gate.app(&f);
    native.start_launch(&b, Some("bob")).await.unwrap();
    f.post(&a, "@bob first").await;
    f.post(&a, "@bob second").await;
    let app = Application::new(
        Arc::new(CrashStore {
            store: f.store.clone(),
            point: "wake_read_probe".into(),
        }),
        f.clock.clone(),
        gate.clone(),
    );
    reads(0, 0, 0);
    let first = app.prepare_wake(&b).await.unwrap().unwrap();
    reads(0, 0, 1);
    assert!(app.prepare_wake(&b).await.unwrap().is_none());
    reads(0, 1, 0);
    assert!(
        app.finish_wake(
            &b,
            &first,
            Ok(HostReceipt {
                turn_id: Some("turn-a".into())
            })
        )
        .await
        .unwrap()
    );
    reads(1, 1, 0);
    assert!(app.prepare_wake(&b).await.unwrap().is_none());
    reads(0, 1, 0);
    f.host.turn(&b, Some("turn-a"));
    app.heartbeat(&b).await.unwrap();
    reads(0, 0, 0);
    f.clock.advance(1);
    assert!(app.prepare_wake(&b).await.unwrap().is_none());
    reads(0, 1, 0);
    f.host.turn(&b, Some("turn-b"));
    app.heartbeat(&b).await.unwrap();
    reads(0, 0, 0);
    let second = app.prepare_wake(&b).await.unwrap().unwrap();
    reads(0, 1, 1);
    assert_eq!(second.hint.events, first.hint.events);
    assert_eq!(
        second.presentation,
        Presentation::Active {
            expected_turn_id: Some("turn-b".into())
        }
    );
    let later = f.post(&a, "@bob behind the stable set").await;
    assert!(app.prepare_wake(&b).await.unwrap().is_none());
    reads(0, 1, 0);
    assert!(app.finish_wake(&b, &second, failure()).await.unwrap());
    reads(1, 1, 0);
    f.host.turn(&b, None);
    app.heartbeat(&b).await.unwrap();
    reads(0, 0, 0);
    f.clock.advance(5);
    let third = app.prepare_wake(&b).await.unwrap().unwrap();
    reads(0, 1, 1);
    assert_eq!(third.hint.events, first.hint.events);
    assert_eq!(third.hint.pending_count, 3);
    assert_eq!(third.attempt, 3);
    assert!(app.finish_wake(&b, &third, failure()).await.unwrap());
    reads(1, 1, 0);
    assert!(app.prepare_wake(&b).await.unwrap().is_none());
    reads(0, 1, 0);
    for reference in &first.hint.events {
        let full = native.prepare_message(&b).await.unwrap().unwrap();
        assert_eq!(full.input.delivery_id, reference.delivery_id);
        native
            .finish_message(&b, &full, Ok(HostReceipt::default()))
            .await
            .unwrap();
    }
    reads(0, 0, 0);
    assert_settled(&f, first.hint.batch_id).await;
    let next = app.prepare_wake(&b).await.unwrap().unwrap();
    // Acceptance already retired the old control in the same transaction;
    // only the newly unassigned event set needs hydration.
    reads(0, 0, 1);
    assert_eq!(
        next.hint.events[0].message_id,
        support::id(&later.result, "message_id")
    );
    assert_eq!(next.attempt, 1);
    assert_ne!(next.hint.batch_id, first.hint.batch_id);

    // Original complete-input acceptance makes the hint set empty and settles
    // its control atomically, before a new preparation hydrates later events.
    let last = f.post(&a, "@bob after complete delivery").await;
    let full = native.prepare_message(&b).await.unwrap().unwrap();
    assert_eq!(full.input.delivery_id, next.hint.events[0].delivery_id);
    native
        .finish_message(&b, &full, Ok(HostReceipt::default()))
        .await
        .unwrap();
    assert_settled(&f, next.hint.batch_id).await;
    let final_attempt = app.prepare_wake(&b).await.unwrap().unwrap();
    reads(0, 0, 1);
    assert_eq!(
        final_attempt.hint.events[0].message_id,
        support::id(&last.result, "message_id")
    );
    assert_ne!(final_attempt.hint.batch_id, next.hint.batch_id);
    assert!(
        !app.finish_wake(&b, &next, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    reads(1, 0, 0);
    assert_eq!(f.count("read_receipts").await, 0);
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.wake_batch(next.hint.batch_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        BatchState::Settled
    );
    tx.rollback().await.unwrap();
    f.store.integrity_check().await.unwrap();
}
