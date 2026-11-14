//! Real shared operations contend at the deleted-root boundary of one fence.
mod support;
use agentchat_core::{Error, ErrorCode, Result, time::SystemClock};
use agentchat_hosts::setup::Environment;
use agentchat_runtime::{
    daemon::{RuntimeOptions, Server},
    removal::{RemovalProgress, RemovalStage},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use support::{Fixture, SyntheticHost};
use uuid::Uuid;

struct Gate {
    reached: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl RemovalProgress for Gate {
    fn reached(&self, stage: RemovalStage) -> Result<()> {
        if stage
            == (RemovalStage::NodeRemoved {
                component: 0,
                node: 0,
            })
            && let Some(sender) = self.reached.lock().unwrap().take()
        {
            sender.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))
                .map_err(|_| {
                    Error::new(ErrorCode::IoError, "Removal fixture release timed out.")
                })?;
        }
        Ok(())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_and_runtime_start_cannot_cross_global_root_deletion_or_recreate_old_history() {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let env = Environment {
        user_config_root: Some(f.config.clone()),
        ..Default::default()
    };
    let (ready, when_ready) = tokio::sync::oneshot::channel();
    let (release, wait) = mpsc::channel();
    let manager = f.manager.clone().with_removal_progress(Arc::new(Gate {
        reached: Mutex::new(Some(ready)),
        release: Mutex::new(wait),
    }));
    let task = tokio::spawn(async move {
        manager
            .uninstall_global_with_environment(Uuid::new_v4(), env)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), when_ready)
        .await
        .unwrap()
        .unwrap();
    assert!(!f.manager.home().exists());
    let (setup, start) = tokio::join!(
        f.manager.initialize(&f.workspace, f.request()),
        Server::bind(
            f.manager.clone(),
            project.clone(),
            Arc::new(SyntheticHost),
            Arc::new(SystemClock),
            RuntimeOptions::default()
        )
    );
    release.send(()).unwrap();
    assert_eq!(setup.unwrap_err().code, ErrorCode::Busy);
    assert_eq!(
        start.err().expect("runtime crossed deletion fence").code,
        ErrorCode::Busy
    );
    task.await.unwrap().unwrap();
    assert!(!f.manager.home().exists());
    assert!(project.reference_path().exists());
    assert_eq!(
        f.manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProjectBindingMismatch
    );
    let data = project.data_dir().to_owned();
    assert_eq!(
        Server::bind(
            f.manager.clone(),
            project,
            Arc::new(SyntheticHost),
            Arc::new(SystemClock),
            RuntimeOptions::default()
        )
        .await
        .err()
        .unwrap()
        .code,
        ErrorCode::ProjectBindingMismatch
    );
    // A refused ordinary install may create its root-level coordination lock;
    // it must never recreate the retained UUID/database or project history.
    assert!(!data.exists());
    assert_eq!(
        fs::read_dir(f.manager.home())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>(),
        vec![std::ffi::OsString::from("init.lock")]
    );
}

#[tokio::test]
async fn neither_scope_can_delete_a_nested_override_fence_or_retired_key_history() {
    use agentchat_hosts::lifecycle::Lifecycle;
    use agentchat_runtime::project::ProjectManager;
    use std::os::unix::fs::MetadataExt;
    for global in [false, true] {
        let f = Fixture::new();
        let outer = f
            .manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap()
            .project;
        let container = if global {
            f.manager.home()
        } else {
            outer.data_dir()
        };
        let nested = ProjectManager::new(container.join("nested-state")).unwrap();
        let workspace = f.root.path().join("independent-project");
        fs::create_dir(&workspace).unwrap();
        let inner = nested
            .initialize(&workspace, f.request())
            .await
            .unwrap()
            .project;
        let keeper = Lifecycle::new(nested.home())
            .unwrap()
            .directory()
            .to_owned();
        let inode = fs::metadata(keeper.join("fence.lock")).unwrap().ino();
        let env = Environment {
            user_config_root: Some(f.config.clone()),
            ..Default::default()
        };
        let result = if global {
            f.manager
                .uninstall_global_with_environment(Uuid::new_v4(), env)
                .await
        } else {
            f.manager
                .uninstall_project_with_environment(&f.workspace, Uuid::new_v4(), env)
                .await
        };
        assert_eq!(result.unwrap_err().code, ErrorCode::ConfigurationConflict);
        assert!(outer.database_path().exists());
        assert!(outer.reference_path().exists());
        assert!(inner.database_path().exists());
        assert!(inner.reference_path().exists());
        assert_eq!(
            fs::metadata(keeper.join("fence.lock")).unwrap().ino(),
            inode
        );
        assert!(
            !Lifecycle::new(f.manager.home())
                .unwrap()
                .directory()
                .join("pending-removal.json")
                .exists()
        );
    }
}

struct ReplaceRoot {
    root: std::path::PathBuf,
    once: Mutex<bool>,
}
impl RemovalProgress for ReplaceRoot {
    fn reached(&self, stage: RemovalStage) -> Result<()> {
        let mut once = self.once.lock().unwrap();
        if stage
            == (RemovalStage::NodeRemoved {
                component: 0,
                node: 0,
            })
            && !*once
        {
            *once = true;
            fs::create_dir(&self.root).unwrap();
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(self.root.join("foreign-replacement"), b"new foreign state").unwrap();
        }
        Ok(())
    }
}
#[tokio::test]
async fn a_foreign_root_replacement_is_preserved_and_requires_exact_recovery_after_reconciliation()
{
    let f = Fixture::new();
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let env = Environment {
        user_config_root: Some(f.config.clone()),
        ..Default::default()
    };
    let manager = f
        .manager
        .clone()
        .with_removal_progress(Arc::new(ReplaceRoot {
            root: f.manager.home().into(),
            once: Mutex::new(false),
        }));
    let id = Uuid::new_v4();
    let first = manager
        .uninstall_global_with_environment(id, env.clone())
        .await
        .unwrap_err();
    assert_eq!(first.code, ErrorCode::ConfigurationConflict);
    assert_eq!(first.details["reason"], "partial_removal");
    assert_eq!(
        fs::read(f.manager.home().join("foreign-replacement")).unwrap(),
        b"new foreign state"
    );
    assert!(
        manager
            .uninstall_global_with_environment(id, env.clone())
            .await
            .is_err()
    );
    let preserved = f.root.path().join("retained-foreign-root");
    fs::rename(f.manager.home(), &preserved).unwrap();
    manager
        .uninstall_global_with_environment(id, env)
        .await
        .unwrap();
    assert_eq!(
        fs::read(preserved.join("foreign-replacement")).unwrap(),
        b"new foreign state"
    );
    assert!(!f.manager.home().exists());
}
