//! Lifecycle-fenced cleanup through actual helper subprocesses, not host admission.
use super::*;
use agentchat_hosts::claude::{NativeLifecycle, NativeRegistration};
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf};

async fn project(f: &Fixture) -> (Value, OwnerNamespace, NativeRegistration) {
    f.initialize().await;
    let intent = reserve(f, Uuid::new_v4()).await;
    let value = register(f, &intent).await;
    let owner: records::LifecycleOwner = serde_json::from_value(value).unwrap();
    let NativeLifecycle::Project(registration) = owner.registration else {
        panic!("expected project lifecycle")
    };
    (intent, namespace(f).await, registration)
}
fn notices(ns: &OwnerNamespace, registration: &NativeRegistration) -> [(PathBuf, Value); 2] {
    let context = &registration.context;
    let token = Uuid::new_v4();
    // Signal generations belong to the cwd, independently of lifecycle order.
    let signal = registration.generation + 997;
    [
        (
            ns.directory()
                .join(format!("wake-{}.json", context.context_id)),
            json!({
                "root":registration.root,"context_id":context.context_id,"incarnation":context.incarnation,
                "generation":signal,"token":token,"expected_turn_id":null,
                "hint":{"version":1,"project_id":context.project_id,"context_id":context.context_id,
                    "incarnation":context.incarnation,"agent_id":Uuid::new_v4(),"batch_id":Uuid::new_v4(),
                    "events":[],"pending_count":0,"preview":"retirement fixture"}
            }),
        ),
        (
            ns.directory()
                .join(format!("submitted-{}.json", context.context_id)),
            json!({
                "accepted":true,"root":registration.root,"context_id":context.context_id,
                "incarnation":context.incarnation,"generation":signal,"token":token,"unix_ms":1
            }),
        ),
    ]
}
fn write_notices(items: &[(PathBuf, Value); 2]) {
    for (path, record) in items {
        records::write(path, record).unwrap();
    }
}
fn header(ns: &OwnerNamespace) -> Value {
    records::read(&ns.directory().join("owner-state.json")).unwrap()
}
fn assert_floors(ns: &OwnerNamespace, before: &Value) {
    let after = header(ns);
    for name in [
        "generation",
        "call_sequence",
        "observation_sequence",
        "lock_identity",
    ] {
        assert_eq!(after[name], before[name], "{name}");
    }
}
#[tokio::test]
async fn exact_retirement_removes_notices_and_retries_without_resetting_history() {
    let f = Fixture::new();
    let (intent, ns, registration) = project(&f).await;
    let items = notices(&ns, &registration);
    write_notices(&items);
    let before = header(&ns);
    let descriptor = f
        .manager
        .home()
        .join("contexts")
        .join(format!("{}.json", registration.context.context_id));
    let immutable = fs::read(&descriptor).unwrap();
    let signal = fs::read(f.workspace.join(".agentchat-wakeup")).unwrap();
    assert_eq!(
        callback(&f, "retire", scope(&intent)).await["retired"],
        true
    );
    assert!(items.iter().all(|(p, _)| !p.exists()));
    assert!(ns.current().await.unwrap().is_none());
    assert_floors(&ns, &before);
    // A repeated end callback may finish leftover projections without reviving.
    write_notices(&items);
    assert_eq!(
        callback(&f, "retire", scope(&intent)).await["retired"],
        false
    );
    assert!(items.iter().all(|(p, _)| !p.exists()));
    assert_floors(&ns, &before);
    assert_eq!(fs::read(descriptor).unwrap(), immutable);
    assert_eq!(
        fs::read(f.workspace.join(".agentchat-wakeup")).unwrap(),
        signal
    );
}
#[tokio::test]
async fn retired_notice_identity_mismatches_are_preserved() {
    for field in ["root", "context_id", "incarnation"] {
        let f = Fixture::new();
        let (intent, ns, registration) = project(&f).await;
        let mut items = notices(&ns, &registration);
        for (_, value) in &mut items {
            value[field] = json!(Uuid::new_v4());
        }
        write_notices(&items);
        let before: Vec<_> = items
            .iter()
            .map(|(p, _)| (fs::read(p).unwrap(), fs::metadata(p).unwrap().ino()))
            .collect();
        callback(&f, "retire", scope(&intent)).await;
        for ((path, _), (bytes, inode)) in items.iter().zip(before) {
            assert_eq!(fs::read(path).unwrap(), bytes, "{field}");
            assert_eq!(fs::metadata(path).unwrap().ino(), inode, "{field}");
        }
        assert!(ns.current().await.unwrap().is_none());
    }
}
#[tokio::test]
async fn stale_end_cannot_delete_same_root_successor_notices() {
    let f = Fixture::new();
    let (old, ns, registration) = project(&f).await;
    callback(&f, "retire", scope(&old)).await;
    let intent = reserve(&f, registration.root).await;
    let value = register(&f, &intent).await;
    let owner: records::LifecycleOwner = serde_json::from_value(value).unwrap();
    let NativeLifecycle::Project(next) = owner.registration else {
        panic!("project")
    };
    assert_ne!(next.context.context_id, registration.context.context_id);
    let items = notices(&ns, &next);
    write_notices(&items);
    let before = header(&ns);
    assert_eq!(callback(&f, "retire", scope(&old)).await["retired"], false);
    assert_eq!(header(&ns), before);
    for (path, record) in items {
        assert_eq!(records::read::<Value>(&path).unwrap(), record);
    }
    assert_eq!(
        ns.current().await.unwrap().unwrap().lifecycle,
        NativeLifecycle::Project(next)
    );
}
#[tokio::test]
async fn partial_retirement_refuses_malformed_receipt_and_can_finish_on_retry() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    let (intent, ns, registration) = project(&f).await;
    let items = notices(&ns, &registration);
    write_notices(&items);
    let original = fs::symlink_metadata(&items[1].0).unwrap();
    fs::write(&items[1].0, b"{interrupted").unwrap();
    let before = header(&ns);
    let result = input(
        &f,
        &serde_json::to_vec(&json!({"action":"retire","input":scope(&intent)})).unwrap(),
    )
    .await;
    assert_eq!(result.status.code(), Some(4));
    assert!(!items[0].0.exists());
    assert_eq!(fs::read(&items[1].0).unwrap(), b"{interrupted");
    assert_eq!(header(&ns), before);
    // Explicitly repair only our known original corrupted fixture. The product
    // must not adopt malformed/unproved data for automatic overwrite.
    let current = fs::symlink_metadata(&items[1].0).unwrap();
    assert_eq!(
        (current.dev(), current.ino()),
        (original.dev(), original.ino())
    );
    fs::remove_file(&items[1].0).unwrap();
    records::write(&items[1].0, &items[1].1).unwrap();
    callback(&f, "retire", scope(&intent)).await;
    assert!(items.iter().all(|(p, _)| !p.exists()));
    assert!(ns.current().await.unwrap().is_none());
    assert_floors(&ns, &before);
}
#[tokio::test]
async fn aliased_retirement_records_are_refused_and_left_untouched() {
    for hard_link in [false, true] {
        let f = Fixture::new();
        let (intent, ns, registration) = project(&f).await;
        let items = notices(&ns, &registration);
        write_notices(&items);
        let target = ns.directory().join("foreign-sentinel.json");
        fs::rename(&items[0].0, &target).unwrap();
        if hard_link {
            fs::hard_link(&target, &items[0].0).unwrap();
        } else {
            std::os::unix::fs::symlink(&target, &items[0].0).unwrap();
        }
        let bytes = fs::read(&target).unwrap();
        let inode = fs::symlink_metadata(&items[0].0).unwrap().ino();
        let before = header(&ns);
        let result = input(
            &f,
            &serde_json::to_vec(&json!({"action":"retire","input":scope(&intent)})).unwrap(),
        )
        .await;
        assert_eq!(result.status.code(), Some(4));
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(fs::symlink_metadata(&items[0].0).unwrap().ino(), inode);
        assert_eq!(header(&ns), before);
        fs::remove_file(&items[0].0).unwrap();
        records::write(&items[0].0, &items[0].1).unwrap();
        callback(&f, "retire", scope(&intent)).await;
        assert!(items.iter().all(|(p, _)| !p.exists()));
        assert_eq!(fs::read(target).unwrap(), bytes);
    }
}
#[tokio::test]
async fn publication_and_retirement_share_one_fence() {
    let f = Fixture::new();
    f.initialize().await;
    let ns = namespace(&f).await;
    for _ in 0..12 {
        let intent = reserve(&f, Uuid::new_v4()).await;
        let value = register(&f, &intent).await;
        let owner: records::LifecycleOwner = serde_json::from_value(value).unwrap();
        let NativeLifecycle::Project(registration) = owner.registration else {
            panic!("project")
        };
        let items = notices(&ns, &registration);
        write_notices(&items);
        let write = items.clone();
        let ticket = serde_json::from_value(intent.clone()).unwrap();
        let (published, _) = tokio::join!(
            ns.publish_current(ticket, move |_| {
                for (path, value) in write {
                    records::write(&path, &value)?;
                }
                Ok(())
            }),
            callback(&f, "retire", scope(&intent))
        );
        let _ = published; // Either ordering is valid; neither may leave a notice.
        assert!(items.iter().all(|(p, _)| !p.exists()));
        assert!(ns.current().await.unwrap().is_none());
    }
}
