//! Exact opaque descriptors preserve isolation without implicit participation.

mod support;
use agentchat_core::ErrorCode;
use agentchat_runtime::context::ContextRegistry;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use support::{Fixture, runtime::seed};
use uuid::Uuid;

#[tokio::test]
async fn unrelated_lifecycle_and_unknown_context_remain_inert() {
    let f = Fixture::new();
    let registry = ContextRegistry::new(f.manager.clone());
    assert_eq!(
        registry
            .issue(seed(&f, "exact-unjoined"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotInitialized
    );
    assert_eq!(
        registry
            .resolve(Uuid::new_v4(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    assert!(!f.manager.home().exists());
    assert!(!f.workspace.join(".agentchat").exists());
}

#[tokio::test]
async fn descriptors_are_private_fresh_and_reject_namespace_replacement() {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let registry = ContextRegistry::new(f.manager.clone());
    let before = fs::read(project.database_path()).unwrap();
    let a = registry.issue(seed(&f, "root-a")).await.unwrap();
    let b = registry.issue(seed(&f, "root-b")).await.unwrap();
    let resumed = registry.issue(seed(&f, "root-a")).await.unwrap();
    assert_ne!(a.context_id, b.context_id);
    assert_ne!(a.context_id, resumed.context_id);
    assert_ne!(a.incarnation, resumed.incarnation);
    assert_eq!(a.host_session_id, resumed.host_session_id);
    assert_eq!(a.working_directory, b.working_directory);
    let resolved = registry
        .resolve(a.context_id, Some(project.identity().id))
        .await
        .unwrap();
    assert_eq!(resolved.context_id, a.context_id);
    assert_eq!(resolved.host_session_id, "root-a");
    assert_eq!(fs::read(project.database_path()).unwrap(), before);
    let directory = f.manager.home().join("contexts");
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(directory.join(format!("{}.json", a.context_id)))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        registry
            .resolve(a.context_id, Some(Uuid::new_v4()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProjectBindingMismatch
    );
    let mut request = f.request();
    request.new_project = true;
    f.manager.initialize(&f.workspace, request).await.unwrap();
    assert_eq!(
        registry.resolve(a.context_id, None).await.unwrap_err().code,
        ErrorCode::ProjectBindingMismatch
    );
    assert!(project.database_path().exists());
}

#[tokio::test]
async fn incompatible_or_exposed_descriptors_and_foreign_symlinks_are_refused() {
    let f = Fixture::new();
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let registry = ContextRegistry::new(f.manager.clone());
    let context = registry.issue(seed(&f, "root-a")).await.unwrap();
    let path = f
        .manager
        .home()
        .join("contexts")
        .join(format!("{}.json", context.context_id));
    let original = fs::read(&path).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["version"] = 2.into();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        registry
            .resolve(context.context_id, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedProtocol
    );
    fs::write(&path, &original).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        registry
            .resolve(context.context_id, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    fs::remove_file(&path).unwrap();
    let foreign = f.root.path().join("foreign.json");
    fs::write(&foreign, &original).unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&foreign, &path).unwrap();
    assert_eq!(
        registry
            .resolve(context.context_id, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(&foreign).unwrap(), original);
}
