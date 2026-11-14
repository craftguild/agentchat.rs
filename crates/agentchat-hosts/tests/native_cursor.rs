//! Persistent metadata is routing state only; these are not host admission tests.
use agentchat_core::ErrorCode;
use agentchat_hosts::claude::{
    NativeBootstrap, NativeLifecycle,
    namespace::{OwnerNamespace, Reservation},
    process::ProcessIdentity,
};
use serde_json::Value;
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};
use uuid::Uuid;

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
async fn namespace(root: &tempfile::TempDir) -> OwnerNamespace {
    OwnerNamespace::open(
        root.path(),
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .await
    .unwrap()
}
fn lifecycle(ns: &OwnerNamespace, r: &Reservation) -> NativeLifecycle {
    NativeLifecycle::Bootstrap(NativeBootstrap {
        owner_root: ns.id(),
        observer: ns.observer().into(),
        generation: r.generation,
        root: r.root,
        lifecycle_id: Uuid::new_v4(),
        host_version: "2.1.301".into(),
        os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        working_directory: r.working_directory.clone(),
    })
}
async fn active(ns: &OwnerNamespace) -> Reservation {
    let intent = ns
        .reserve(Uuid::new_v4(), PathBuf::from("/"), Some(Uuid::new_v4()))
        .await
        .unwrap();
    ns.commit(intent.clone(), lifecycle(ns, &intent))
        .await
        .unwrap();
    intent
}
#[tokio::test]
async fn reopen_preserves_generation_sequences_and_exact_idempotent_commit() {
    let r = root();
    let ns = namespace(&r).await;
    let intent = active(&ns).await;
    let committed = ns.current().await.unwrap().unwrap();
    assert!(
        !ns.commit(intent.clone(), committed.lifecycle.clone())
            .await
            .unwrap()
    );
    assert_eq!(
        ns.call_sequence(intent.root, intent.generation)
            .await
            .unwrap(),
        1
    );
    let observed = ns
        .observation_sequence(intent.root, intent.generation)
        .await
        .unwrap();
    assert!(observed.current);
    assert_eq!(observed.sequence, 1);
    let again = namespace(&r).await;
    assert_eq!(again.current().await.unwrap(), Some(committed));
    assert_eq!(
        again
            .call_sequence(intent.root, intent.generation)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        again
            .observation_sequence(intent.root, intent.generation)
            .await
            .unwrap()
            .sequence,
        2
    );
    let next = again
        .reserve(intent.root, PathBuf::from("/"), Some(Uuid::new_v4()))
        .await
        .unwrap();
    assert_eq!(next.generation, intent.generation + 1);
    assert!(again.current().await.unwrap().is_none());
    assert!(!r.path().join("contexts").exists());
}
#[tokio::test]
async fn superseded_async_tickets_and_old_end_callbacks_cannot_steal_the_successor() {
    let r = root();
    let ns = namespace(&r).await;
    let old = active(&ns).await;
    let a = ns.reserve(Uuid::new_v4(), "/".into(), None).await.unwrap();
    let b = ns.reserve(Uuid::new_v4(), "/".into(), None).await.unwrap();
    assert!(ns.commit(a.clone(), lifecycle(&ns, &a)).await.is_err());
    assert!(!ns.retire(old.root, old.generation).await.unwrap());
    assert!(!ns.retire(a.root, a.generation).await.unwrap());
    assert!(ns.commit(b.clone(), lifecycle(&ns, &b)).await.unwrap());
    assert_eq!(ns.current().await.unwrap().unwrap().reservation, b);
    assert!(!ns.retire(old.root, old.generation).await.unwrap());
}
#[tokio::test]
async fn cancelling_a_transition_or_retiring_current_never_revives_old_authority() {
    let r = root();
    let ns = namespace(&r).await;
    let old = active(&ns).await;
    let old_lifecycle = ns.current().await.unwrap().unwrap().lifecycle;
    let next = ns.reserve(Uuid::new_v4(), "/".into(), None).await.unwrap();
    assert!(ns.call_sequence(old.root, old.generation).await.is_err());
    assert!(
        !ns.observation_sequence(old.root, old.generation)
            .await
            .unwrap()
            .current
    );
    assert!(ns.retire(next.root, next.generation).await.unwrap());
    assert!(ns.current().await.unwrap().is_none());
    assert!(ns.commit(old.clone(), old_lifecycle.clone()).await.is_err());
    let third = ns.reserve(old.root, "/".into(), None).await.unwrap();
    let mut reused = old_lifecycle;
    let NativeLifecycle::Bootstrap(ref mut registration) = reused else {
        unreachable!()
    };
    registration.generation = third.generation;
    assert!(ns.commit(third.clone(), reused).await.is_err());
    assert!(
        ns.commit(third.clone(), lifecycle(&ns, &third))
            .await
            .unwrap()
    );
    assert!(ns.retire(third.root, third.generation).await.unwrap());
    assert!(!ns.retire(third.root, third.generation).await.unwrap());
    assert!(ns.current().await.unwrap().is_none());
}
#[tokio::test]
async fn concurrent_operations_allocate_unique_positive_sequences() {
    let r = root();
    let ns = namespace(&r).await;
    let current = active(&ns).await;
    let calls = futures_util::future::join_all(
        (0..32).map(|_| retry_busy(|| ns.call_sequence(current.root, current.generation))),
    )
    .await;
    let mut numbers = calls.into_iter().map(Result::unwrap).collect::<Vec<_>>();
    numbers.sort();
    assert_eq!(numbers, (1..=32).collect::<Vec<_>>());
    let reservations = futures_util::future::join_all((0..8).map(|_| {
        let ns = &ns;
        let root = Uuid::new_v4();
        retry_busy(move || ns.reserve(root, "/".into(), None))
    }))
    .await;
    let mut generations = reservations
        .into_iter()
        .map(|x| x.unwrap().generation)
        .collect::<Vec<_>>();
    generations.sort();
    assert_eq!(generations, (2..=9).collect::<Vec<_>>());
}
async fn retry_busy<T, F, Fut>(mut operation: F) -> agentchat_core::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = agentchat_core::Result<T>>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match operation().await {
            Err(error)
                if error.code == ErrorCode::Busy
                    && error.retryable
                    && tokio::time::Instant::now() < deadline =>
            {
                // Busy precedes the mutation. Retry only that documented result;
                // correctness under contention must not require 32 fsyncs/2s.
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            result => return result,
        }
    }
}
#[tokio::test]
async fn invalid_native_facts_do_not_consume_a_generation_or_change_the_record() {
    let r = root();
    let ns = namespace(&r).await;
    let path = ns.directory().join("owner-state.json");
    let before = std::fs::read(&path).unwrap();
    for (id, cwd, nonce) in [
        (Uuid::nil(), PathBuf::from("/"), None),
        (Uuid::new_v4(), PathBuf::from("relative"), None),
        (Uuid::new_v4(), PathBuf::from("/"), Some(Uuid::nil())),
    ] {
        assert!(ns.reserve(id, cwd, nonce).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    assert_eq!(active(&ns).await.generation, 1);
}
#[tokio::test]
async fn mismatched_callback_identity_or_path_cannot_commit_reserved_generation() {
    let r = root();
    let ns = namespace(&r).await;
    let intent = ns.reserve(Uuid::new_v4(), "/".into(), None).await.unwrap();
    for field in [
        "owner_root",
        "observer",
        "root",
        "generation",
        "working_directory",
    ] {
        let mut value = serde_json::to_value(lifecycle(&ns, &intent)).unwrap();
        value["registration"][field] = match field {
            "observer" => Value::from("foreign-observer"),
            "generation" => Value::from(intent.generation + 1),
            "working_directory" => Value::from("/different"),
            _ => Value::from(Uuid::new_v4().to_string()),
        };
        assert!(
            ns.commit(intent.clone(), serde_json::from_value(value).unwrap())
                .await
                .is_err()
        );
        assert!(ns.current().await.unwrap().is_none());
    }
    assert!(
        ns.commit(intent.clone(), lifecycle(&ns, &intent))
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn changed_or_unknown_header_refuses_without_repairing_foreign_bytes() {
    let r = root();
    let ns = namespace(&r).await;
    active(&ns).await;
    let path = ns.directory().join("owner-state.json");
    let before = std::fs::read(&path).unwrap();
    for field in [
        "version",
        "namespace_id",
        "observer",
        "lock_identity",
        "generation",
    ] {
        let mut value: Value = serde_json::from_slice(&before).unwrap();
        value[field] = match field {
            "version" => Value::from(99),
            "namespace_id" => Value::from(Uuid::new_v4().to_string()),
            "observer" => Value::from("foreign"),
            "lock_identity" => serde_json::json!([0, 0]),
            _ => Value::from(0),
        };
        let changed = serde_json::to_vec(&value).unwrap();
        std::fs::write(&path, &changed).unwrap();
        assert!(ns.current().await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), changed);
        std::fs::write(&path, &before).unwrap();
    }
}
#[tokio::test]
async fn javascript_integer_exhaustion_refuses_without_wrapping_any_floor() {
    let r = root();
    let ns = namespace(&r).await;
    let current = active(&ns).await;
    let path = ns.directory().join("owner-state.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for field in ["call_sequence", "observation_sequence"] {
        value[field] = Value::from(9_007_199_254_740_991u64);
    }
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(
        ns.call_sequence(current.root, current.generation)
            .await
            .is_err()
    );
    assert!(
        ns.observation_sequence(current.root, current.generation)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    value["generation"] = Value::from(9_007_199_254_740_991u64);
    value["retired"] = Value::Bool(true);
    let exhausted = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &exhausted).unwrap();
    assert!(ns.reserve(Uuid::new_v4(), "/".into(), None).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), exhausted);
}
#[tokio::test]
async fn an_unsafe_or_missing_cursor_cannot_be_silently_reinitialized() {
    let r = root();
    let ns = namespace(&r).await;
    let path = ns.directory().join("owner-state.json");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(ns.current().await.is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = ns.directory().join("foreign.json");
    std::fs::rename(&path, &link).unwrap();
    std::os::unix::fs::symlink(&link, &path).unwrap();
    assert!(ns.current().await.is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&link, &path).unwrap();
    assert!(ns.current().await.is_err());
    std::fs::remove_file(&path).unwrap();
    assert_eq!(ns.current().await.unwrap_err().code, ErrorCode::Busy);
    assert!(!path.exists());
    assert_eq!(std::fs::read(&link).unwrap(), bytes);
    std::fs::rename(&link, &path).unwrap();
    std::fs::write(&path, vec![b' '; 262145]).unwrap();
    assert!(ns.current().await.is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 262145);
}
#[tokio::test]
async fn replacing_the_lock_inode_cannot_split_the_authority_floor() {
    let r = root();
    let ns = namespace(&r).await;
    let lock = ns.directory().join("owner-state.lock");
    std::fs::rename(&lock, ns.directory().join("old.lock")).unwrap();
    assert!(namespace_result(&r).await.is_err());
    assert!(
        !lock.exists(),
        "a missing established lock must not be replaced"
    );
}
async fn namespace_result(r: &tempfile::TempDir) -> agentchat_core::Result<OwnerNamespace> {
    OwnerNamespace::open(
        r.path(),
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .await
}

#[test]
#[ignore = "subprocess used explicitly by process-serialization/crash tests"]
fn cursor_process_child() {
    let r = PathBuf::from(std::env::var_os("AGENTCHAT_TEST_CURSOR_HOME").unwrap());
    let pid = std::env::var("AGENTCHAT_TEST_CURSOR_OWNER")
        .unwrap()
        .parse()
        .unwrap();
    let output = PathBuf::from(std::env::var_os("AGENTCHAT_TEST_CURSOR_OUTPUT").unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ns = runtime
        .block_on(OwnerNamespace::open(
            &r,
            ProcessIdentity::capture(pid).unwrap(),
        ))
        .unwrap();
    if std::env::var_os("AGENTCHAT_TEST_CURSOR_HOLD").is_some() {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(ns.directory().join("owner-state.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&file).unwrap();
        std::fs::write(&output, b"locked").unwrap();
        loop {
            std::thread::park();
        }
    }
    let intent = runtime
        .block_on(ns.reserve(Uuid::new_v4(), "/".into(), None))
        .unwrap();
    std::fs::write(output, serde_json::to_vec(&intent).unwrap()).unwrap();
}
fn child(r: &tempfile::TempDir, output: &std::path::Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(std::env::current_exe().unwrap());
    c.args([
        "--ignored",
        "--exact",
        "cursor_process_child",
        "--nocapture",
    ])
    .env("AGENTCHAT_TEST_CURSOR_HOME", r.path())
    .env(
        "AGENTCHAT_TEST_CURSOR_OWNER",
        std::process::id().to_string(),
    )
    .env("AGENTCHAT_TEST_CURSOR_OUTPUT", output)
    .env_remove("AGENTCHAT_TEST_CURSOR_HOLD")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);
    c
}
#[tokio::test]
async fn separate_helpers_serialize_generations_and_a_killed_lock_holder_releases_the_cursor() {
    let r = root();
    let ns = namespace(&r).await;
    let mut children = Vec::new();
    for i in 0..8 {
        let output = r.path().join(format!("child-{i}.json"));
        children.push((child(&r, &output).spawn().unwrap(), output));
    }
    let mut generations = Vec::new();
    for (c, path) in children {
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), c.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        generations.push(
            serde_json::from_slice::<Reservation>(&std::fs::read(path).unwrap())
                .unwrap()
                .generation,
        );
    }
    generations.sort();
    assert_eq!(generations, (1..=8).collect::<Vec<_>>());
    let ready = r.path().join("held");
    let mut holder = child(&r, &ready)
        .env("AGENTCHAT_TEST_CURSOR_HOLD", "1")
        .spawn()
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready.exists() {
        assert!(holder.try_wait().unwrap().is_none());
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        ns.reserve(Uuid::new_v4(), "/".into(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    holder.kill().await.unwrap();
    holder.wait().await.unwrap();
    assert_eq!(
        ns.reserve(Uuid::new_v4(), "/".into(), None)
            .await
            .unwrap()
            .generation,
        9
    );
    assert_eq!(
        std::fs::metadata(ns.directory().join("owner-state.lock"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
}
