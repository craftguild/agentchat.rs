//! Real subprocess exits across durable initialization phases and replay recovery.

mod support;

use agentchat_core::{
    Error, ErrorCode, Result, project::Scope, read::ChannelListCondition, time::Timestamp,
};
use agentchat_runtime::project::{InitProgress, InitRequest, InitStage, ProjectManager};
use serde_json::Value;
use std::{
    fs,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use support::Fixture;

struct ExitAt(InitStage);
impl InitProgress for ExitAt {
    fn reached(&self, stage: InitStage) -> Result<()> {
        if stage == self.0 {
            std::process::exit(86);
        }
        Ok(())
    }
}

#[tokio::test]
#[ignore = "subprocess helper explicitly invoked at every durable cut by its parent"]
async fn init_crash_child() {
    assert_eq!(
        std::env::var_os("HOME"),
        std::env::var_os("AGENTCHAT_EXPECTED_HOME")
    );
    assert_eq!(
        std::env::var_os("CODEX_HOME"),
        std::env::var_os("AGENTCHAT_EXPECTED_CODEX_HOME")
    );
    let stage = serde_json::from_str(&std::env::var("AGENTCHAT_TEST_INIT_STAGE").unwrap()).unwrap();
    let request =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_INIT_REQUEST").unwrap()).unwrap();
    let workspace = std::env::var_os("AGENTCHAT_TEST_INIT_WORKSPACE").unwrap();
    let manager = ProjectManager::from_environment()
        .unwrap()
        .with_progress(Arc::new(ExitAt(stage)));
    manager
        .initialize(std::path::PathBuf::from(workspace), request)
        .await
        .unwrap();
    panic!("Initialization never reached the selected crash cut.");
}

#[tokio::test]
async fn crashes_keep_one_uuid_and_recover_complete_storage_references_and_receipts() {
    for stage in [
        InitStage::Prepared,
        InitStage::DatabaseInitialized,
        InitStage::BindingPublished,
        InitStage::ReferencePublished { index: 0 },
        InitStage::ExclusionsUpdated,
        InitStage::ReceiptPublished,
    ] {
        let f = Fixture::new();
        let request = f.request();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "init_crash_child"])
            .env("AGENTCHAT_HOME", f.manager.home())
            .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
            .env("AGENTCHAT_TEST_INIT_WORKSPACE", &f.workspace)
            .env(
                "AGENTCHAT_TEST_INIT_REQUEST",
                serde_json::to_string(&request).unwrap(),
            )
            .env(
                "AGENTCHAT_TEST_INIT_STAGE",
                serde_json::to_string(&stage).unwrap(),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for (actual, expected) in [
            ("HOME", "AGENTCHAT_EXPECTED_HOME"),
            ("CODEX_HOME", "AGENTCHAT_EXPECTED_CODEX_HOME"),
        ] {
            if let Some(value) = std::env::var_os(actual) {
                command.env(expected, value);
            } else {
                command.env_remove(expected);
            }
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Initialization child stalled at {stage:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(86), "{stage:?}");
        let journal = f.manager.home().join("init/pending.json");
        let pending: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
        let intended = pending["receipt"]["binding"]["identity"]["id"]
            .as_str()
            .unwrap();
        let recovered = f
            .manager
            .initialize(&f.workspace, request.clone())
            .await
            .unwrap();
        assert_eq!(recovered.project.identity().id.to_string(), intended);
        assert!(recovered.created);
        assert!(!journal.exists());
        assert_eq!(
            f.manager.initialize(&f.workspace, request).await.unwrap(),
            recovered
        );
        assert_eq!(
            f.manager.discover(&f.workspace).await.unwrap().unwrap(),
            recovered.project
        );
        let store = f.manager.open(&recovered.project).await.unwrap();
        store.integrity_check().await.unwrap();
        let mut snapshot = agentchat_core::ports::Storage::snapshot(store.as_ref())
            .await
            .unwrap();
        assert_eq!(snapshot.head().get(), 2);
        assert_eq!(
            snapshot
                .channel_policy()
                .await
                .unwrap()
                .unwrap()
                .ordered_defaults(),
            &["dev".to_owned()]
        );
        assert_eq!(snapshot.statistics().await.unwrap().agents, 0);
        assert_eq!(
            snapshot
                .channels(
                    Default::default(),
                    201,
                    ChannelListCondition::All,
                    Timestamp::parse("2026-10-26T00:00:00.000Z").unwrap(),
                )
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(f.config.read_dir().unwrap().next().is_none());
    }
}

struct FailOnce {
    stage: InitStage,
    failed: Mutex<bool>,
}
impl InitProgress for FailOnce {
    fn reached(&self, stage: InitStage) -> Result<()> {
        let mut failed = self.failed.lock().unwrap();
        if stage == self.stage && !*failed {
            *failed = true;
            return Err(Error::new(
                ErrorCode::IoError,
                "Injected progress loss after a durable publication.",
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn interrupted_scope_switch_recovers_before_later_initialization() {
    let f = Fixture::new();
    f.repository(&f.workspace);
    let mut directory = f.request();
    directory.scope = Some(Scope::Directory);
    let old = f.manager.initialize(&f.workspace, directory).await.unwrap();
    let failing = f.manager.clone().with_progress(Arc::new(FailOnce {
        stage: InitStage::ReferencePublished { index: 0 },
        failed: Mutex::new(false),
    }));
    let mut replacement = f.request();
    replacement.new_project = true;
    replacement.scope = Some(Scope::Repository);
    assert_eq!(
        failing
            .initialize(&f.workspace, replacement.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IoError
    );
    let pending: Value =
        serde_json::from_slice(&fs::read(f.manager.home().join("init/pending.json")).unwrap())
            .unwrap();
    let intended = pending["receipt"]["binding"]["identity"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A distinct request must first complete the interrupted reference switch.
    let recovered = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    assert_eq!(recovered.project.identity().id.to_string(), intended);
    assert!(!recovered.created);
    assert!(!old.project.reference_path().exists());
    assert!(old.project.database_path().exists());
    assert_eq!(
        f.manager
            .initialize(&f.workspace, replacement)
            .await
            .unwrap()
            .project,
        recovered.project
    );
}

#[tokio::test]
async fn a_changed_foreign_reference_blocks_recovery_without_overwriting_it() {
    let f = Fixture::new();
    let failing = f.manager.clone().with_progress(Arc::new(FailOnce {
        stage: InitStage::DatabaseInitialized,
        failed: Mutex::new(false),
    }));
    let request = InitRequest::new(uuid::Uuid::new_v4());
    assert_eq!(
        failing
            .initialize(&f.workspace, request.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IoError
    );
    let path = f.workspace.join(".agentchat/project.json");
    fs::write(&path, b"foreign replacement").unwrap();
    assert_eq!(
        f.manager
            .initialize(&f.workspace, request)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(path).unwrap(), b"foreign replacement");
    assert!(f.manager.home().join("init/pending.json").exists());
}
