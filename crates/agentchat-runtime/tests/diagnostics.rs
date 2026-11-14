//! Actual bounded files, failure isolation and runtime-generated redacted records.
mod support;
use agentchat_core::{ErrorCode, application::Mutation};
use agentchat_runtime::diagnostics::{
    Event, FILE_BYTES, FILE_COUNT, NativeOutcome, NativePhase, RuntimeLog,
};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    time::Duration,
};
use support::runtime::{Running, join, mutate, options};
use uuid::Uuid;

fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}
async fn first(root: &Path, project: Uuid) -> Vec<u8> {
    let log = RuntimeLog::open(root, project, Uuid::new_v4())
        .await
        .unwrap();
    assert!(log.sink().record(Event::Started));
    log.close().await.unwrap();
    fs::read(root.join("logs/runtime.jsonl")).unwrap()
}
fn records(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

#[tokio::test]
async fn bounded_whole_record_rotation_discards_only_the_oldest_archive() {
    let root = private_root();
    let project = Uuid::new_v4();
    let line = first(root.path(), project).await;
    let log_dir = root.path().join("logs");
    let full = line.repeat(FILE_BYTES / line.len());
    private(&log_dir.join("runtime.jsonl"), &full);
    let mut earlier: Value = serde_json::from_slice(&line).unwrap();
    earlier["instance_id"] = serde_json::json!(Uuid::new_v4());
    let mut previous = serde_json::to_vec(&earlier).unwrap();
    previous.push(b'\n');
    private(&log_dir.join("runtime.jsonl.1"), &previous);
    earlier["instance_id"] = serde_json::json!(Uuid::new_v4());
    let mut oldest = serde_json::to_vec(&earlier).unwrap();
    oldest.push(b'\n');
    private(&log_dir.join("runtime.jsonl.2"), &oldest);
    let log = RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .unwrap();
    assert!(log.sink().record(Event::Started));
    log.close().await.unwrap();
    assert_eq!(fs::read(log_dir.join("runtime.jsonl.1")).unwrap(), full);
    assert_eq!(fs::read(log_dir.join("runtime.jsonl.2")).unwrap(), previous);
    assert_eq!(records(&log_dir.join("runtime.jsonl")).len(), 1);
    assert_eq!(fs::read_dir(&log_dir).unwrap().count(), FILE_COUNT + 1);
    let mut total = 0;
    for entry in fs::read_dir(&log_dir).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.metadata().unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert!(meta.len() <= FILE_BYTES as u64);
        total += meta.len();
    }
    assert!(total <= (FILE_COUNT * FILE_BYTES) as u64);
}

#[tokio::test]
async fn killed_append_tail_and_rotation_gaps_recover_without_old_history_replay() {
    for gap in [None, Some("runtime.jsonl"), Some("runtime.jsonl.1")] {
        let root = private_root();
        let project = Uuid::new_v4();
        let line = first(root.path(), project).await;
        let dir = root.path().join("logs");
        private(&dir.join("runtime.jsonl.1"), &line);
        let mut torn = line.clone();
        torn.extend_from_slice(b"{\"version\":1,\"project_id\":");
        private(&dir.join("runtime.jsonl"), &torn);
        if let Some(gap) = gap {
            fs::remove_file(dir.join(gap)).unwrap();
        }
        let log = RuntimeLog::open(root.path(), project, Uuid::new_v4())
            .await
            .unwrap();
        assert!(log.sink().record(Event::Stopped));
        log.close().await.unwrap();
        let rows = records(&dir.join("runtime.jsonl"));
        assert_eq!(rows.len(), if gap == Some("runtime.jsonl") { 1 } else { 2 });
        assert_eq!(rows.last().unwrap()["event"]["kind"], "stopped");
    }
}

#[tokio::test]
async fn invalid_or_foreign_files_are_preserved_before_repairing_any_tail() {
    for case in 0..8 {
        let root = private_root();
        let project = Uuid::new_v4();
        let line = first(root.path(), project).await;
        let dir = root.path().join("logs");
        let mut torn = line.clone();
        torn.extend_from_slice(b"{\"version\":");
        private(&dir.join("runtime.jsonl"), &torn);
        let bad = dir.join("runtime.jsonl.1");
        match case {
            0 => private(&bad, b"foreign\n"),
            1 => private(&bad, &vec![b'x'; FILE_BYTES + 1]),
            2 => {
                let mut row: Value = serde_json::from_slice(&line).unwrap();
                row["project_id"] = serde_json::json!(Uuid::new_v4());
                private(&bad, &serde_json::to_vec(&row).unwrap());
            }
            3 => {
                private(&bad, &line);
                fs::set_permissions(&bad, fs::Permissions::from_mode(0o644)).unwrap();
            }
            4 => {
                fs::create_dir(&bad).unwrap();
            }
            5 => {
                let foreign = root.path().join("outside");
                private(&foreign, b"test-only private state");
                symlink(&foreign, &bad).unwrap();
            }
            6 => {
                let foreign = root.path().join("outside");
                private(&foreign, &line);
                fs::hard_link(&foreign, &bad).unwrap();
            }
            7 => private(&dir.join("writer.lock"), b"foreign"),
            _ => unreachable!(),
        }
        assert!(
            RuntimeLog::open(root.path(), project, Uuid::new_v4())
                .await
                .is_err()
        );
        assert_eq!(fs::read(dir.join("runtime.jsonl")).unwrap(), torn);
        if case == 5 {
            assert_eq!(
                fs::read(root.path().join("outside")).unwrap(),
                b"test-only private state"
            );
        }
    }
}

#[tokio::test]
async fn one_writer_is_fenced_and_dropping_owner_stops_despite_retained_sinks() {
    let root = private_root();
    let project = Uuid::new_v4();
    let log = RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .unwrap();
    let sink = log.sink();
    assert!(sink.record(Event::Started));
    let error = RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::Busy);
    assert!(error.retryable);
    drop(log);
    tokio::time::timeout(Duration::from_secs(2), async {
        while sink.status().state != "closed" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!sink.record(Event::Stopped));
    assert_eq!(sink.status().dropped_records, 1);
    RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn producer_saturation_is_nonblocking_and_has_bounded_redacted_output() {
    let root = private_root();
    let project = Uuid::new_v4();
    let log = RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .unwrap();
    let sink = log.sink();
    let started = std::time::Instant::now();
    let mut full = false;
    for _ in 0..100_000 {
        if !sink.record(Event::RequestFailed {
            code: ErrorCode::StorageError,
        }) {
            full = true;
            break;
        }
    }
    assert!(full);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(sink.status().dropped_records >= 1);
    log.close().await.unwrap();
    assert_eq!(sink.status().state, "closed");
    for row in records(&root.path().join("logs/runtime.jsonl")) {
        assert_eq!(
            row["event"],
            serde_json::json!({"kind":"request_failed","code":"storage_error"})
        );
        assert_eq!(row.as_object().unwrap().len(), 6);
        assert!(row["runtime_elapsed_us"].is_u64());
    }
}

#[tokio::test]
async fn pipeline_records_fit_the_fixed_budget_and_old_records_still_open() {
    let root = private_root();
    let project = Uuid::new_v4();
    let log = RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .unwrap();
    let sink = log.sink();
    let seq = agentchat_core::sequence::Sequence::new(i64::MAX).unwrap();
    for event in [
        Event::RpcTiming {
            request_id: Some(Uuid::new_v4()),
            queue_us: u64::MAX,
            execution_us: u64::MAX,
        },
        Event::MutationCommitted {
            request_id: Uuid::new_v4(),
            seq,
        },
        Event::NativeInput {
            delivery_id: Uuid::new_v4(),
            seq,
            phase: NativePhase::AcceptanceCommitted,
            receipt_only: false,
            duration_us: u64::MAX,
            outcome: Some(NativeOutcome::AwaitingAcceptance),
        },
    ] {
        assert!(sink.record(event));
    }
    log.close().await.unwrap();
    let path = root.path().join("logs/runtime.jsonl");
    let rows = records(&path);
    assert_eq!(rows.len(), 3);
    for mut row in rows {
        row["runtime_elapsed_us"] = serde_json::json!(u64::MAX);
        row["at_unix_ms"] = serde_json::json!(u64::MAX);
        assert!(serde_json::to_vec(&row).unwrap().len() < sink.status().record_bytes);
    }
    // Log format v1 existed before monotonic telemetry. Missing new diagnostic
    // fields are compatible; no historical timing or business state is invented.
    let mut old = records(&path).remove(0);
    old.as_object_mut().unwrap().remove("runtime_elapsed_us");
    let mut bytes = serde_json::to_vec(&old).unwrap();
    bytes.push(b'\n');
    private(&path, &bytes);
    let log = RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .unwrap();
    assert!(log.sink().record(Event::Stopped));
    log.close().await.unwrap();
    let reopened = records(&path);
    assert_eq!(reopened[0], old);
    assert!(reopened[1]["runtime_elapsed_us"].is_u64());
}

#[tokio::test]
async fn runtime_logs_fixed_events_and_logging_failure_never_undoes_a_post() {
    let r = Running::new(options()).await;
    let context = r.context("private-session-sentinel").await;
    let mut client = r.client().await;
    join(&mut client, &context, "alice").await;
    let body = "PRIVATE_BODY_SENTINEL_TEST_ONLY";
    mutate(
        &mut client,
        &context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: body.into(),
        },
    )
    .await;
    let status = support::runtime::health(&mut client).await;
    assert_eq!(status["runtime"]["diagnostics"]["state"], "running");
    let path = r.project.data_dir().join("logs/runtime.jsonl");
    // A noncooperating same-user replacement must disable diagnostics, while
    // the already-committed application remains usable and keeps its data.
    fs::rename(&path, path.with_file_name("replaced-test-log")).unwrap();
    private(&path, b"foreign replacement\n");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            support::runtime::health(&mut client).await;
            if r.handle.diagnostics().state == "failed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    mutate(
        &mut client,
        &context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: body.into(),
        },
    )
    .await;
    assert_eq!(
        support::runtime::health(&mut client).await["statistics"]["messages"],
        2
    );
    assert_eq!(fs::read(&path).unwrap(), b"foreign replacement\n");
    let original = fs::read(path.with_file_name("replaced-test-log")).unwrap();
    let workspace = r.f.workspace.to_string_lossy();
    for secret in [body, "private-session-sentinel", workspace.as_ref()] {
        if !secret.is_empty() {
            assert!(!String::from_utf8_lossy(&original).contains(secret));
        }
    }
    assert!(r.handle.diagnostics().dropped_records > 0);
    r.stop().await;
}

#[tokio::test]
async fn graceful_restart_retains_instance_events_and_doctor_reports_live_log_health() {
    use agentchat_protocol::operations::{Backend, Operation, Target};
    use agentchat_runtime::{
        operations::{Launcher, Operations},
        project::Project,
    };
    use std::sync::Arc;
    struct NeverLaunch;
    #[async_trait::async_trait]
    impl Launcher for NeverLaunch {
        async fn launch(&self, _: &Project, _: &Path) -> agentchat_core::Result<()> {
            panic!("doctor must not launch a daemon");
        }
    }
    let mut r = Running::new(options()).await;
    let old = r.handle.instance_id();
    let mut client = r.client().await;
    let status = support::runtime::health(&mut client).await;
    assert_eq!(status["runtime"]["diagnostics"]["file_bytes"], FILE_BYTES);
    assert_eq!(status["runtime"]["diagnostics"]["file_count"], FILE_COUNT);
    drop(client);
    r.restart(options()).await;
    let rows = records(&r.project.data_dir().join("logs/runtime.jsonl"));
    let old = rows
        .iter()
        .filter(|r| r["instance_id"] == old.to_string())
        .collect::<Vec<_>>();
    assert_eq!(old.first().unwrap()["event"]["kind"], "started");
    assert_eq!(old.last().unwrap()["event"]["kind"], "stopped");
    let backend = Operations::new(r.f.manager.clone(), Arc::new(NeverLaunch));
    let doctor = backend
        .invoke(
            Uuid::new_v4(),
            Target {
                project_path: Some(r.f.workspace.clone()),
                ..Default::default()
            },
            Operation::Doctor,
        )
        .await;
    let doctor = serde_json::to_value(doctor).unwrap();
    assert_eq!(
        doctor["result"]["runtime"]["diagnostics"]["state"], "running",
        "{doctor}"
    );
    assert_eq!(
        doctor["result"]["runtime"]["diagnostics"]["dropped_records"],
        0
    );
    assert_eq!(doctor["result"]["diagnostic_logs"]["state"], "running");
    r.stop().await;
}

#[tokio::test]
async fn offline_inspection_is_read_only_and_never_repairs_or_creates_logs() {
    use agentchat_runtime::diagnostics::inspect;
    let root = private_root();
    let project = Uuid::new_v4();
    assert_eq!(
        inspect(root.path(), project).await.unwrap()["state"],
        "absent"
    );
    assert!(!root.path().join("logs").exists());
    let log = RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        inspect(root.path(), project).await.unwrap()["state"],
        "busy"
    );
    log.sink().record(Event::Started);
    log.close().await.unwrap();
    let file = root.path().join("logs/runtime.jsonl");
    let mut bytes = fs::read(&file).unwrap();
    let tail = b"{\"version\":";
    bytes.extend_from_slice(tail);
    private(&file, &bytes);
    let before = fs::metadata(&file).unwrap().modified().unwrap();
    let result = inspect(root.path(), project).await.unwrap();
    assert_eq!(result["state"], "valid");
    assert_eq!(result["incomplete_tail_bytes"], tail.len());
    assert_eq!(fs::read(&file).unwrap(), bytes);
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), before);
    private(
        &root.path().join("logs/runtime.jsonl.1"),
        b"invalid complete record\n",
    );
    assert!(inspect(root.path(), project).await.is_err());
    assert_eq!(fs::read(&file).unwrap(), bytes);
    let absent = root.path().join("never-created");
    let other = private_root();
    symlink(&absent, other.path().join("logs")).unwrap();
    assert!(inspect(other.path(), project).await.is_err());
    assert!(!absent.exists());
}

#[tokio::test]
async fn foreign_archive_replacement_stops_rotation_before_deleting_retained_files() {
    let root = private_root();
    let project = Uuid::new_v4();
    let line = first(root.path(), project).await;
    let dir = root.path().join("logs");
    let full = line.repeat(FILE_BYTES / line.len());
    private(&dir.join("runtime.jsonl"), &full);
    private(&dir.join("runtime.jsonl.1"), &line);
    private(&dir.join("runtime.jsonl.2"), &line);
    let log = RuntimeLog::open(root.path(), project, Uuid::new_v4())
        .await
        .unwrap();
    let sink = log.sink();
    fs::rename(
        dir.join("runtime.jsonl.1"),
        dir.join("test-owned-previous-archive"),
    )
    .unwrap();
    private(&dir.join("runtime.jsonl.1"), b"foreign replacement\n");
    assert!(sink.record(Event::Started));
    assert!(log.close().await.is_err());
    assert_eq!(sink.status().state, "failed");
    assert_eq!(fs::read(dir.join("runtime.jsonl")).unwrap(), full);
    assert_eq!(
        fs::read(dir.join("runtime.jsonl.1")).unwrap(),
        b"foreign replacement\n"
    );
    assert_eq!(fs::read(dir.join("runtime.jsonl.2")).unwrap(), line);
}
