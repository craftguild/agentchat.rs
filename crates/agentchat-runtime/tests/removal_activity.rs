//! Removal observes viewer ownership even while the project runtime is stopped.
mod support;

use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::lifecycle::Lifecycle;
use agentchat_runtime::{
    daemon::{RuntimeOptions, Server},
    operations::{Launcher, Operations},
    project::Project,
};
use async_trait::async_trait;
use std::{path::Path, sync::Arc, time::Duration};
use support::{
    SyntheticHost,
    runtime::{Running, options},
};
use uuid::Uuid;

#[tokio::test]
async fn actual_local_and_global_removal_preserve_live_runtime_and_disconnected_viewer_then_succeed_after_close()
 {
    let r = Running::new(options()).await;
    let environment = agentchat_hosts::setup::Environment {
        user_config_root: Some(r.f.config.clone()),
        ..Default::default()
    };
    for global in [false, true] {
        let result = if global {
            r.f.manager
                .uninstall_global_with_environment(Uuid::new_v4(), environment.clone())
                .await
        } else {
            r.f.manager
                .uninstall_project_with_environment(
                    &r.f.workspace,
                    Uuid::new_v4(),
                    environment.clone(),
                )
                .await
        };
        assert_eq!(result.unwrap_err().code, ErrorCode::Busy);
        assert!(r.project.database_path().exists());
    }
    let operations = Operations::new(r.f.manager.clone(), Arc::new(NoLaunch));
    let viewer = operations.viewer(r.f.workspace.clone()).await.unwrap();
    stop_owner(&r).await;
    for global in [false, true] {
        let result = if global {
            r.f.manager
                .uninstall_global_with_environment(Uuid::new_v4(), environment.clone())
                .await
        } else {
            r.f.manager
                .uninstall_project_with_environment(
                    &r.f.workspace,
                    Uuid::new_v4(),
                    environment.clone(),
                )
                .await
        };
        assert_eq!(result.unwrap_err().code, ErrorCode::Busy);
        assert!(r.project.database_path().exists());
    }
    viewer.close().await;
    r.f.manager
        .uninstall_global_with_environment(Uuid::new_v4(), environment)
        .await
        .unwrap();
    assert!(!r.f.manager.home().exists());
    assert!(r.project.reference_path().exists());
    r.stop().await;
}

struct NoLaunch;
#[async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "The fixture runtime is explicitly stopped.",
        ))
    }
}

async fn stop_owner(r: &Running) {
    r.handle.shutdown();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !r.finished() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn live_viewer_retains_removal_authority_fence_during_runtime_loss_and_releases_on_close() {
    let r = Running::new(options()).await;
    let lifecycle = Lifecycle::new(r.f.manager.home()).unwrap();
    let id = r.project.identity().id;
    {
        let fence = lifecycle.exclusive().unwrap();
        assert_eq!(
            lifecycle.quiescent(&fence, Some(id)).err().unwrap().code,
            ErrorCode::Busy
        );
    }
    let operations = Operations::new(r.f.manager.clone(), Arc::new(NoLaunch));
    let viewer = operations.viewer(r.f.workspace.clone()).await.unwrap();
    stop_owner(&r).await;
    let fence = lifecycle.exclusive().unwrap();
    assert_eq!(
        lifecycle.quiescent(&fence, Some(id)).err().unwrap().code,
        ErrorCode::Busy
    );
    viewer.close().await;
    let held = lifecycle.quiescent(&fence, Some(id)).unwrap();
    assert_eq!(held.len(), 1);
    drop(held);
    drop(fence);
    r.stop().await;
}

#[tokio::test]
async fn runtime_start_is_bounded_by_removal_and_cannot_recreate_a_deleted_retained_namespace() {
    let r = Running::new(options()).await;
    stop_owner(&r).await;
    let lifecycle = Lifecycle::new(r.f.manager.home()).unwrap();
    let fence = lifecycle.exclusive().unwrap();
    std::fs::remove_dir_all(r.f.manager.home()).unwrap();
    let result = Server::bind(
        r.f.manager.clone(),
        r.project.clone(),
        Arc::new(SyntheticHost),
        r.clock.clone(),
        RuntimeOptions::default(),
    )
    .await;
    let error = match result {
        Ok(_) => panic!("startup crossed removal fence"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Busy);
    assert!(error.retryable);
    assert!(!r.f.manager.home().exists());
    drop(fence);
    let result = Server::bind(
        r.f.manager.clone(),
        r.project.clone(),
        Arc::new(SyntheticHost),
        r.clock.clone(),
        RuntimeOptions::default(),
    )
    .await;
    let error = match result {
        Ok(_) => panic!("startup recreated removed history"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::ProjectBindingMismatch);
    assert!(!r.f.manager.home().exists());
    assert!(r.project.reference_path().exists());
    r.stop().await;
}
