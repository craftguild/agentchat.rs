//! Real mixed callback/mutation cuts retain all original claims or the whole commit.
use super::*;
use agentchat_core::{
    application::{CommitOutcome, CommitRequest, MutationRequest},
    delivery::WakeCompletion,
};
use serde_json::{Value, json};

fn request(context: &Context, key: Uuid) -> MutationRequest {
    MutationRequest {
        request_id: key,
        context: context.clone(),
        command: Mutation::Post {
            channel: Some("dev".into()),
            body: "mixed callback and mutation durability".into(),
        },
    }
}
fn inputs(context: &Context, key: Uuid, callbacks: &[WakeCompletion]) -> Vec<CommitRequest> {
    vec![
        CommitRequest::WakeCompletion(callbacks[0].clone()),
        CommitRequest::Mutation(request(context, key)),
        CommitRequest::WakeCompletion(callbacks[1].clone()),
    ]
}
pub(super) async fn run(app: &Application, context: &Context, key: Uuid, point: &str) {
    let data: Vec<Value> =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_COMPLETIONS").unwrap()).unwrap();
    let mut callbacks = data
        .into_iter()
        .map(completions::decode)
        .collect::<Vec<_>>();
    if point == "group_after_error_rollback" {
        callbacks[1].outcome = Ok(HostReceipt {
            turn_id: Some(String::new()),
        });
    }
    let result = app.commit_ready(inputs(context, key, &callbacks)).await;
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
    let result = result.unwrap();
    assert!(matches!(result[0], Ok(CommitOutcome::WakeCompletion(true))));
    assert!(matches!(result[1], Ok(CommitOutcome::Mutation(_))));
    assert!(matches!(result[2], Ok(CommitOutcome::WakeCompletion(true))));
    if point == "completion_pause_second_batch" {
        std::process::exit(0);
    }
}

struct Child(std::process::Child);
impl Child {
    fn start(
        f: &Fixture,
        actor: &Context,
        key: Uuid,
        callbacks: &[WakeCompletion],
        point: &str,
    ) -> Self {
        let data = callbacks.iter().map(|c| json!({"context":c.context,"hint":c.dispatch.hint,
            "token":c.dispatch.token,"attempt":c.dispatch.attempt,"turn":c.outcome.as_ref().unwrap().turn_id})).collect::<Vec<_>>();
        Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "crash_child"])
                .env("AGENTCHAT_TEST_CRASH_ACTION", "ready_commits")
                .env(
                    "AGENTCHAT_TEST_CRASH_DB",
                    f.root.path().join("agentchat.sqlite3"),
                )
                .env(
                    "AGENTCHAT_TEST_CRASH_CONTEXT",
                    serde_json::to_string(actor).unwrap(),
                )
                .env("AGENTCHAT_TEST_CRASH_REQUEST", key.to_string())
                .env("AGENTCHAT_TEST_CRASH_POINT", point)
                .env(
                    "AGENTCHAT_TEST_COMPLETIONS",
                    serde_json::to_string(&data).unwrap(),
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
            assert!(Instant::now() < deadline, "mixed child did not exit");
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

async fn retry(
    f: &Fixture,
    actor: &Context,
    key: Uuid,
    callbacks: &[WakeCompletion],
    committed: bool,
) {
    let reopened = Arc::new(
        Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let app = Application::new(reopened.clone(), f.clock.clone(), f.host.clone());
    let result = app
        .commit_ready(inputs(actor, key, callbacks))
        .await
        .unwrap();
    assert!(matches!(result[0], Ok(CommitOutcome::WakeCompletion(value)) if value != committed));
    assert!(matches!(result[2], Ok(CommitOutcome::WakeCompletion(value)) if value != committed));
    let original = match &result[1] {
        Ok(CommitOutcome::Mutation(value)) => value,
        _ => panic!("missing recovered mutation result"),
    };
    assert_eq!(original.event_head.is_some(), !committed);
    let mutation = request(actor, key);
    let replay = app.execute(key, actor, mutation.command).await.unwrap();
    assert_eq!(replay.result, original.result);
    assert!(replay.event_head.is_none());
    for callback in callbacks {
        let mut tx = reopened.begin().await.unwrap();
        let batch = tx
            .wake_batch(callback.dispatch.hint.batch_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.state, BatchState::Inflight);
        assert_eq!(batch.attempts, 1);
        assert_eq!(batch.dispatch_ref, Some(callback.dispatch.token));
        tx.rollback().await.unwrap();
    }
    assert_eq!(f.count("dispatch_messages").await, 2);
    assert_eq!(f.count("read_receipts").await, 0);
    reopened.integrity_check().await.unwrap();
}

#[tokio::test]
async fn eleven_mixed_process_cuts_preserve_both_callback_state_and_original_mutation_key() {
    for point in [
        "group_before_release_1",
        "group_after_release_1",
        "after_state",
        "group_after_release_2",
        "group_after_error_rollback",
        "group_before_outer_commit",
        "group_reject_outer_commit",
        "completion_fail_second_batch",
        "completion_corrupt_second_batch",
        "completion_io_second_batch",
        "group_after_outer_commit",
    ] {
        let (f, actor, callbacks) = completions::fixture().await;
        let before = completions::rows(&f).await;
        let key = Uuid::new_v4();
        assert_eq!(
            Child::start(&f, &actor, key, &callbacks, point)
                .wait()
                .code(),
            Some(86),
            "{point}"
        );
        let committed = point == "group_after_outer_commit";
        assert_eq!(
            f.count("dispatch_messages").await,
            if committed { 2 } else { 1 }
        );
        if !committed {
            assert_eq!(completions::rows(&f).await, before, "{point}");
        }
        retry(&f, &actor, key, &callbacks, committed).await;
    }
}

#[tokio::test]
async fn mixed_savepoints_are_invisible_until_outer_commit_and_abrupt_exit_restores_all() {
    for cancel in [false, true] {
        let (f, actor, callbacks) = completions::fixture().await;
        let before = completions::rows(&f).await;
        let key = Uuid::new_v4();
        let mut child = Child::start(&f, &actor, key, &callbacks, "completion_pause_second_batch");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !f.root.path().join("ready").exists() {
                assert!(child.0.try_wait().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(completions::rows(&f).await, before);
        assert_eq!(f.count("dispatch_messages").await, 1);
        if cancel {
            child.0.kill().unwrap();
            assert!(!child.wait().success());
        } else {
            std::fs::write(f.root.path().join("continue"), b"continue").unwrap();
            assert!(child.wait().success());
        }
        if cancel {
            assert_eq!(completions::rows(&f).await, before);
        }
        retry(&f, &actor, key, &callbacks, !cancel).await;
    }
}
