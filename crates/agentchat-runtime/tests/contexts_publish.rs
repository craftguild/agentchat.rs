//! Immutable publication reuses actual random allocations without host authority.
mod support;
use agentchat_core::{ErrorCode, model::Context};
use agentchat_runtime::context::ContextRegistry;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};
use support::{Fixture, runtime::seed};
use uuid::Uuid;
#[tokio::test]
async fn pure_preparation_and_exact_publication_keep_one_private_immutable_record() {
    let f = Fixture::new();
    let registry = ContextRegistry::new(f.manager.clone());
    assert_eq!(
        registry.prepare(seed(&f, "root-a")).await.unwrap_err().code,
        ErrorCode::NotInitialized
    );
    assert!(!f.manager.home().exists());
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let first = registry.prepare(seed(&f, "root-a")).await.unwrap();
    assert!(!f.manager.home().join("contexts").exists());
    assert_eq!(first.context_id.get_version(), Some(uuid::Version::Random));
    assert_eq!(first.incarnation.get_version(), Some(uuid::Version::Random));
    let second = registry.prepare(seed(&f, "root-a")).await.unwrap();
    assert_ne!(first.context_id, second.context_id);
    assert_ne!(first.incarnation, second.incarnation);
    assert_eq!(registry.publish(first.clone()).await.unwrap(), first);
    let path = f
        .manager
        .home()
        .join("contexts")
        .join(format!("{}.json", first.context_id));
    let bytes = fs::read(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    let fresh = ContextRegistry::new(f.manager.clone());
    for _ in 0..16 {
        assert_eq!(fresh.publish(first.clone()).await.unwrap(), first);
    }
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    assert_eq!(fresh.publish(second.clone()).await.unwrap(), second);
    let ordinary = registry.issue(seed(&f, "root-a")).await.unwrap();
    assert_ne!(ordinary.context_id, first.context_id);
    assert_ne!(ordinary.incarnation, first.incarnation);
}
#[tokio::test]
async fn simultaneous_publications_reuse_one_complete_allocation_without_extra_index_files() {
    let f = Fixture::new();
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let registry = ContextRegistry::new(f.manager.clone());
    let context = registry.prepare(seed(&f, "root-a")).await.unwrap();
    let mut jobs = Vec::new();
    for _ in 0..32 {
        let r = registry.clone();
        let c = context.clone();
        jobs.push(tokio::spawn(async move { r.publish(c).await.unwrap() }));
    }
    for job in jobs {
        assert_eq!(job.await.unwrap(), context);
    }
    assert_eq!(
        fs::read_dir(f.manager.home().join("contexts"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        registry.descriptor(context.context_id).await.unwrap(),
        context
    );
}
#[tokio::test]
async fn different_roots_racing_the_same_allocated_reference_cannot_rebind_it() {
    let f = Fixture::new();
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let registry = ContextRegistry::new(f.manager.clone());
    let first = registry.prepare(seed(&f, "root-a")).await.unwrap();
    let mut other = first.clone();
    other.host_session_id = "root-b".into();
    let (a, b) = tokio::join!(registry.publish(first), registry.publish(other));
    let (winner, loser) = match (a, b) {
        (Ok(w), Err(e)) | (Err(e), Ok(w)) => (w, e),
        other => panic!("Expected one immutable winner: {other:?}"),
    };
    assert_eq!(loser.code, ErrorCode::IdempotencyConflict);
    assert_eq!(
        registry.descriptor(winner.context_id).await.unwrap(),
        winner
    );
    assert_eq!(
        fs::read_dir(f.manager.home().join("contexts"))
            .unwrap()
            .count(),
        1
    );
}
#[tokio::test]
async fn changed_descriptor_facts_conflict_and_stale_project_publication_refuses() {
    let f = Fixture::new();
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let registry = ContextRegistry::new(f.manager.clone());
    let original = registry.issue(seed(&f, "root-a")).await.unwrap();
    let path = f
        .manager
        .home()
        .join("contexts")
        .join(format!("{}.json", original.context_id));
    let bytes = fs::read(&path).unwrap();
    for changed in [
        {
            let mut c = original.clone();
            c.host_session_id = "other-root".into();
            c
        },
        {
            let mut c = original.clone();
            c.host_version = "different-version".into();
            c
        },
        {
            let mut c = original.clone();
            c.os_profile = "different-os".into();
            c
        },
    ] {
        assert_eq!(
            registry.publish(changed).await.unwrap_err().code,
            ErrorCode::IdempotencyConflict
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    let mut request = f.request();
    request.new_project = true;
    f.manager.initialize(&f.workspace, request).await.unwrap();
    assert_eq!(
        registry.publish(original.clone()).await.unwrap_err().code,
        ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(
        registry.descriptor(original.context_id).await.unwrap(),
        original
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}
#[tokio::test]
async fn incomplete_references_and_foreign_symbolic_or_exposed_records_are_preserved() {
    let f = Fixture::new();
    let registry = ContextRegistry::new(f.manager.clone());
    let s = seed(&f, "root-a");
    let nil = Context {
        context_id: Uuid::nil(),
        incarnation: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        host: s.host,
        host_session_id: s.host_session_id,
        host_version: s.host_version,
        os_profile: s.os_profile,
        working_directory: s.working_directory,
    };
    assert_eq!(
        registry.publish(nil).await.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(!f.manager.home().exists());
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let context = registry.issue(seed(&f, "root-a")).await.unwrap();
    let path = f
        .manager
        .home()
        .join("contexts")
        .join(format!("{}.json", context.context_id));
    let original = fs::read(&path).unwrap();
    let mut future: serde_json::Value = serde_json::from_slice(&original).unwrap();
    future["version"] = 99.into();
    let bytes = serde_json::to_vec(&future).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        registry.publish(context.clone()).await.unwrap_err().code,
        ErrorCode::UnsupportedProtocol
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    fs::write(&path, &original).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        registry.publish(context.clone()).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&path).unwrap();
    let missing = f.root.path().join("foreign-missing");
    symlink(&missing, &path).unwrap();
    assert_eq!(
        registry.publish(context).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert!(fs::symlink_metadata(path).unwrap().is_symlink() && !missing.exists());
}
