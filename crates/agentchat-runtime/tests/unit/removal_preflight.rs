//! Disposable authority and receipt proof before removal publication is added.

use super::*;
use crate::project::InitRequest;
use agentchat_core::ErrorCode;
use agentchat_hosts::lifecycle::Lifecycle;

#[tokio::test]
async fn retired_success_cannot_restore_reference_or_namespace_after_root_deletion() {
    let fixture = tempfile::tempdir().unwrap();
    let base = fixture.path().canonicalize().unwrap();
    let root = base.join("state");
    let workspace = base.join("workspace");
    fs::create_dir(&workspace).unwrap();
    let manager = ProjectManager::new(&root).unwrap();
    let request = InitRequest::new(Uuid::new_v4());
    let installed = manager
        .initialize(&workspace, request.clone())
        .await
        .unwrap();
    let other = base.join("other");
    fs::create_dir(&other).unwrap();
    let second_request = InitRequest::new(Uuid::new_v4());
    let second = manager
        .initialize(&other, second_request.clone())
        .await
        .unwrap();
    {
        let lifecycle = Lifecycle::new(&root).unwrap();
        let _fence = lifecycle.exclusive().unwrap();
        retire(&manager, Some(installed.project.identity().id)).unwrap();
        assert_eq!(
            check(&manager, request.request_id).unwrap_err().code,
            ErrorCode::IdempotencyConflict
        );
        check(&manager, second_request.request_id).unwrap();
        fs::remove_file(installed.project.reference_path()).unwrap();
        fs::remove_dir_all(installed.project.data_dir()).unwrap();
    }
    assert_eq!(
        manager
            .initialize(&workspace, request.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert!(!installed.project.reference_path().exists());
    assert!(!installed.project.data_dir().exists());
    assert_eq!(
        manager
            .initialize(&other, second_request.clone())
            .await
            .unwrap(),
        second
    );
    {
        let lifecycle = Lifecycle::new(&root).unwrap();
        let _fence = lifecycle.exclusive().unwrap();
        retire(&manager, None).unwrap();
        fs::remove_dir_all(&root).unwrap();
        fs::create_dir(&root).unwrap();
    }
    for (directory, request) in [(&workspace, request), (&other, second_request)] {
        assert_eq!(
            manager
                .initialize(directory, request.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::IdempotencyConflict
        );
        assert_eq!(
            manager
                .resume_initialization(directory, request)
                .await
                .unwrap_err()
                .code,
            ErrorCode::IdempotencyConflict
        );
    }
    assert!(
        second.project.reference_path().exists(),
        "global reset retains references"
    );
    assert!(fs::read_dir(&root).unwrap().next().is_none());
    let refused = manager
        .initialize(&other, InitRequest::new(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::ProjectBindingMismatch);
    assert_eq!(refused.details["reason"], "retained_namespace_missing");
    assert!(refused.message.contains("agentchat install --new-project"));
    assert!(
        refused
            .message
            .contains("agentchat uninstall followed by agentchat install")
    );
    assert!(!second.project.data_dir().exists());
}

#[tokio::test]
async fn copied_reference_never_authorizes_the_original_namespace() {
    let fixture = tempfile::tempdir().unwrap();
    let base = fixture.path().canonicalize().unwrap();
    let original = base.join("original");
    let copied = base.join("copied");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&copied).unwrap();
    let manager = ProjectManager::new(base.join("state")).unwrap();
    let installed = manager
        .initialize(&original, InitRequest::new(Uuid::new_v4()))
        .await
        .unwrap();
    fs::create_dir(copied.join(".agentchat")).unwrap();
    fs::copy(
        installed.project.reference_path(),
        copied.join(".agentchat/project.json"),
    )
    .unwrap();
    assert_eq!(
        manager.discover(&copied).await.unwrap_err().code,
        ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(
        manager.discover(&original).await.unwrap().unwrap(),
        installed.project
    );
    assert!(installed.project.database_path().exists());
}
