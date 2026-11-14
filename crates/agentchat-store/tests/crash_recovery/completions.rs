//! Native outcomes stay provisional through every savepoint and outer failure.
use super::*;
use agentchat_core::delivery::{Presentation, WakeCompletion, WakeDispatch};
use serde_json::{Value, json};

pub(super) fn pause() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AGENTCHAT_TEST_COMPLETION_ROOT").unwrap());
    std::fs::write(root.join("ready"), b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("continue").exists() {
        assert!(
            Instant::now() < deadline,
            "completion parent did not release child"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
pub(super) fn decode(value: Value) -> WakeCompletion {
    WakeCompletion {
        context: serde_json::from_value(value["context"].clone()).unwrap(),
        dispatch: WakeDispatch {
            hint: serde_json::from_value(value["hint"].clone()).unwrap(),
            token: Uuid::parse_str(value["token"].as_str().unwrap()).unwrap(),
            attempt: value["attempt"].as_u64().unwrap().try_into().unwrap(),
            presentation: Presentation::Idle,
        },
        outcome: Ok(HostReceipt {
            turn_id: Some(value["turn"].as_str().unwrap().into()),
        }),
    }
}
pub(super) async fn run(app: &Application, point: &str) {
    let input: Vec<Value> =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_COMPLETIONS").unwrap()).unwrap();
    let mut callbacks = input.into_iter().map(decode).collect::<Vec<_>>();
    if point == "group_after_error_rollback" {
        callbacks[1].outcome = Ok(HostReceipt {
            turn_id: Some(String::new()),
        });
    }
    let result = app.finish_wakes(callbacks).await;
    if matches!(
        point,
        "group_reject_outer_commit"
            | "completion_fail_second_batch"
            | "completion_corrupt_second_batch"
            | "completion_io_second_batch"
    ) {
        assert_eq!(
            result.unwrap_err().code,
            match point {
                "completion_corrupt_second_batch" => agentchat_core::ErrorCode::IntegrityError,
                "completion_io_second_batch" => agentchat_core::ErrorCode::IoError,
                _ => agentchat_core::ErrorCode::StorageError,
            }
        );
        std::process::exit(86);
    }
    assert_eq!(result.unwrap(), vec![Ok(true), Ok(true)]);
    if point == "completion_pause_second_batch" {
        std::process::exit(0);
    }
}
struct Child(std::process::Child);
impl Child {
    fn start(f: &Fixture, context: &Context, callbacks: &[WakeCompletion], point: &str) -> Self {
        let input = callbacks
            .iter()
            .map(|c| {
                assert!(matches!(c.dispatch.presentation, Presentation::Idle));
                json!({"context":c.context,"hint":c.dispatch.hint,"token":c.dispatch.token,
                "attempt":c.dispatch.attempt,"turn":c.outcome.as_ref().unwrap().turn_id})
            })
            .collect::<Vec<_>>();
        Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "crash_child"])
                .env("AGENTCHAT_TEST_CRASH_ACTION", "completion_group")
                .env(
                    "AGENTCHAT_TEST_CRASH_DB",
                    f.root.path().join("agentchat.sqlite3"),
                )
                .env(
                    "AGENTCHAT_TEST_CRASH_CONTEXT",
                    serde_json::to_string(context).unwrap(),
                )
                .env("AGENTCHAT_TEST_CRASH_REQUEST", Uuid::new_v4().to_string())
                .env("AGENTCHAT_TEST_CRASH_POINT", point)
                .env(
                    "AGENTCHAT_TEST_COMPLETIONS",
                    serde_json::to_string(&input).unwrap(),
                )
                .env("AGENTCHAT_TEST_COMPLETION_ROOT", f.root.path())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }
    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "completion child exceeded deadline"
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
pub(super) async fn fixture() -> (Fixture, Context, Vec<WakeCompletion>) {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    f.join(&c, "cara").await;
    f.post(&a, "@bob @cara atomic completion controls").await;
    let mut callbacks = Vec::new();
    for (context, turn) in [(b, "turn-b"), (c, "turn-c")] {
        callbacks.push(WakeCompletion {
            dispatch: f.app.prepare_wake(&context).await.unwrap().unwrap(),
            context,
            outcome: Ok(HostReceipt {
                turn_id: Some(turn.into()),
            }),
        });
    }
    (f, a, callbacks)
}
pub(super) async fn rows(f: &Fixture) -> Vec<Value> {
    let mut rows = f
        .rows("wake_batches")
        .await
        .into_iter()
        .map(|r| {
            json!({
                "id":r.try_get::<String>("","id").unwrap(),
                "recipient":r.try_get::<String>("","recipient_id").unwrap(),
                "incarnation":r.try_get::<String>("","incarnation").unwrap(),
                "state":r.try_get::<String>("","state").unwrap(),
                "token":r.try_get::<Option<String>>("","dispatch_ref").unwrap(),
                "turn":r.try_get::<Option<String>>("","turn_ref").unwrap(),
                "attempts":r.try_get::<i64>("","attempts").unwrap(),
                "next":r.try_get::<Option<String>>("","next_eligible_at").unwrap(),
                "failure":r.try_get::<Option<String>>("","failure_code").unwrap(),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|r| r["id"].as_str().unwrap().to_owned());
    rows
}

#[tokio::test]
async fn twelve_completion_process_cuts_preserve_original_claims_or_all_recorded_outcomes() {
    for point in [
        "group_before_release_1",
        "group_after_release_1",
        "group_after_error_rollback",
        "completion_second_batch",
        "group_before_release_2",
        "group_after_release_2",
        "group_before_outer_commit",
        "group_reject_outer_commit",
        "completion_fail_second_batch",
        "completion_corrupt_second_batch",
        "completion_io_second_batch",
        "group_after_outer_commit",
    ] {
        let (f, a, callbacks) = fixture().await;
        let before = rows(&f).await;
        let counts = f.counts().await;
        let mut child = Child::start(&f, &a, &callbacks, point);
        assert_eq!(child.wait().code(), Some(86), "{point}");
        let recovered = Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap();
        recovered.integrity_check().await.unwrap();
        recovered.close().await.unwrap();
        let committed = point == "group_after_outer_commit";
        if !committed {
            assert_eq!(rows(&f).await, before, "{point}");
        }
        assert_eq!(f.counts().await, counts, "{point}");
        let mut tx = f.store.begin().await.unwrap();
        for callback in &callbacks {
            let batch = tx
                .wake_batch(callback.dispatch.hint.batch_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                batch.state,
                if committed {
                    BatchState::Inflight
                } else {
                    BatchState::Pending
                },
                "{point}"
            );
            assert_eq!(batch.attempts, 1);
            assert_eq!(batch.dispatch_ref, Some(callback.dispatch.token));
            assert_eq!(
                batch.turn_ref,
                if committed {
                    callback.outcome.as_ref().unwrap().turn_id.clone()
                } else {
                    None
                }
            );
        }
        tx.rollback().await.unwrap();
        assert_eq!(
            f.app.finish_wakes(callbacks.clone()).await.unwrap(),
            vec![Ok(!committed); 2]
        );
        assert_eq!(
            f.app.finish_wakes(callbacks).await.unwrap(),
            vec![Ok(false); 2]
        );
        assert_eq!(f.count("read_receipts").await, 0);
        assert_eq!(f.counts().await, counts);
        f.store.integrity_check().await.unwrap();
    }
}

#[tokio::test]
async fn released_callback_savepoints_are_invisible_and_abrupt_cancellation_rolls_back_all() {
    for cancel in [false, true] {
        let (f, a, callbacks) = fixture().await;
        let before = rows(&f).await;
        let mut child = Child::start(&f, &a, &callbacks, "completion_pause_second_batch");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.root.path().join("ready").exists() {
                assert!(child.0.try_wait().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // The first release and second save both happened, but the independent
        // ORM reader must still observe complete original claims without turns.
        assert_eq!(rows(&f).await, before);
        if cancel {
            child.0.kill().unwrap();
            assert!(!child.wait().success());
        } else {
            std::fs::write(f.root.path().join("continue"), b"continue").unwrap();
            assert!(child.wait().success());
        }
        if cancel {
            assert_eq!(rows(&f).await, before);
        }
        assert_eq!(
            f.app.finish_wakes(callbacks).await.unwrap(),
            vec![Ok(cancel); 2]
        );
        f.store.integrity_check().await.unwrap();
    }
}
