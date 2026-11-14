//! Bulk receipt INSERTs survive process loss without changing watches or events.
use super::*;
use agentchat_core::{ErrorCode, time::Clock};
use serde_json::json;

pub(super) enum Work {
    SingleMessage,
    Messages,
    SingleReceipt,
    Receipts,
    SingleInsert,
    InsertSet,
}
static COUNTERS: [AtomicUsize; 6] = [const { AtomicUsize::new(0) }; 6];
pub(super) fn record(point: &str, work: Work) {
    if point == "read_probe" {
        COUNTERS[work as usize].fetch_add(1, Ordering::SeqCst);
    }
}
pub(super) async fn run(app: &Application, context: &Context, request: Uuid, point: &str) {
    let ids = serde_json::from_str(&std::env::var("AGENTCHAT_TEST_READ_IDS").unwrap()).unwrap();
    let result = app
        .execute(request, context, Mutation::Read { message_ids: ids })
        .await;
    if point == "read_fail_after_insert" {
        assert_eq!(result.unwrap_err().code, ErrorCode::StorageError);
        std::process::exit(86);
    }
    result.unwrap();
}
async fn prepare(f: &Fixture, count: usize, watched: bool) -> (Context, Uuid, Vec<Uuid>) {
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    f.timeout(if watched { 10 } else { 60 }).await;
    let mut ids = Vec::new();
    for i in 0..count {
        let result = f
            .run(
                &a,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: format!("@bob first read {i}"),
                },
            )
            .await;
        ids.push(support::id(&result.result, "message_id"));
    }
    ids.sort_unstable();
    (b, bob, ids)
}

#[tokio::test]
async fn a_twenty_message_read_uses_two_set_lookups_and_one_insert_without_single_row_calls() {
    let f = Fixture::new().await;
    let (b, _, ids) = prepare(&f, 20, false).await;
    let app = Application::new(
        Arc::new(CrashStore {
            store: f.store.clone(),
            point: "read_probe".into(),
        }),
        f.clock.clone(),
        f.host.clone(),
    );
    for counter in &COUNTERS {
        assert_eq!(counter.swap(0, Ordering::SeqCst), 0);
    }
    let result = app
        .execute(
            Uuid::new_v4(),
            &b,
            Mutation::Read {
                message_ids: ids.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.result, json!({"changed":ids,"already_read":[]}));
    assert_eq!(
        COUNTERS.each_ref().map(|c| c.swap(0, Ordering::SeqCst)),
        [0, 1, 0, 1, 0, 1]
    );
    assert_eq!(f.count("read_receipts").await, 20);
    app.execute(Uuid::new_v4(), &b, Mutation::Read { message_ids: ids })
        .await
        .unwrap();
    assert_eq!(
        COUNTERS.each_ref().map(|c| c.swap(0, Ordering::SeqCst)),
        [0, 1, 0, 1, 0, 0],
        "already-read sets perform no insertion or watch lookup"
    );
}

struct Child(std::process::Child);
impl Child {
    fn start(f: &Fixture, context: &Context, request: Uuid, ids: &[Uuid], point: &str) -> Self {
        Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "crash_child"])
                .env("AGENTCHAT_TEST_CRASH_ACTION", "read_set")
                .env(
                    "AGENTCHAT_TEST_CRASH_DB",
                    f.root.path().join("agentchat.sqlite3"),
                )
                .env(
                    "AGENTCHAT_TEST_CRASH_CONTEXT",
                    serde_json::to_string(context).unwrap(),
                )
                .env("AGENTCHAT_TEST_CRASH_REQUEST", request.to_string())
                .env("AGENTCHAT_TEST_CRASH_POINT", point)
                .env(
                    "AGENTCHAT_TEST_READ_IDS",
                    serde_json::to_string(ids).unwrap(),
                )
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }
    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "first-read child exceeded deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn five_real_read_cuts_preserve_receipts_events_retry_keys_and_delivery_claims() {
    for point in [
        "read_before_insert",
        "read_after_insert",
        "before_commit",
        "read_fail_after_insert",
        "after_commit",
    ] {
        let f = Fixture::new().await;
        let (b, bob, ids) = prepare(&f, 20, true).await;
        f.run(
            &b,
            Mutation::Read {
                message_ids: ids[..1].to_vec(),
            },
        )
        .await;
        let first = f.clock.now();
        f.clock.advance(2);
        let dispatch = f.app.prepare_wake(&b).await.unwrap().unwrap();
        f.app
            .finish_wake(&b, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap();
        let events = f.count("events").await;
        let mut tx = f.store.begin().await.unwrap();
        let before_receipts = tx.receipts(bob, &ids).await.unwrap();
        let assigned = dispatch
            .hint
            .events
            .iter()
            .map(|r| r.delivery_id)
            .collect::<Vec<_>>();
        let mut before_deliveries = tx.deliveries(&assigned).await.unwrap();
        before_deliveries.sort_by_key(|r| r.id);
        let before_batch = tx
            .wake_batch(dispatch.hint.batch_id)
            .await
            .unwrap()
            .unwrap();
        tx.rollback().await.unwrap();
        let key = Uuid::new_v4();
        let mut child = Child::start(&f, &b, key, &ids, point);
        assert_eq!(child.wait().code(), Some(86), "{point}");
        let reopened = Arc::new(
            Store::open(
                f.root.path().join("agentchat.sqlite3"),
                f.store.identity().clone(),
            )
            .await
            .unwrap(),
        );
        reopened.integrity_check().await.unwrap();
        let mut tx = reopened.begin().await.unwrap();
        let receipts = tx.receipts(bob, &ids).await.unwrap();
        let committed = point == "after_commit";
        assert_eq!(
            tx.idempotency(key).await.unwrap().is_some(),
            committed,
            "{point}"
        );
        if committed {
            assert_eq!(receipts.len(), 20);
        } else {
            assert_eq!(receipts, before_receipts, "{point}");
        }
        let mut deliveries = tx.deliveries(&assigned).await.unwrap();
        deliveries.sort_by_key(|r| r.id);
        assert_eq!(deliveries, before_deliveries);
        assert_eq!(
            tx.wake_batch(before_batch.id).await.unwrap().unwrap(),
            before_batch
        );
        tx.rollback().await.unwrap();
        assert_eq!(f.count("events").await, events);
        let app = Application::new(reopened.clone(), f.clock.clone(), f.host.clone());
        let command = Mutation::Read {
            message_ids: ids.clone(),
        };
        let result = app.execute(key, &b, command.clone()).await.unwrap();
        assert_eq!(
            result.result,
            json!({"changed":ids[1..],"already_read":ids[..1]})
        );
        assert_eq!(
            app.execute(key, &b, command).await.unwrap().result,
            result.result
        );
        let mut tx = reopened.begin().await.unwrap();
        for r in tx.receipts(bob, &ids).await.unwrap() {
            assert_eq!(
                r.first_read_at,
                if r.message_id == ids[0] {
                    first
                } else {
                    f.clock.now()
                }
            );
        }
        tx.rollback().await.unwrap();
        assert_eq!(f.count("events").await, events);
        reopened.integrity_check().await.unwrap();
    }
}
