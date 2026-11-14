//! Real process cuts span project publication, host setup and success receipts.
use agentchat_core::{Result, project::Scope};
use agentchat_hosts::{
    install::{InstallProgress, InstallStage},
    setup::{Environment, ToolSetup},
};
use agentchat_protocol::operations::{Backend, Operation, Target};
use agentchat_runtime::{
    operations::{Launcher, Operations},
    project::{InitProgress, InitStage, Project, ProjectManager},
};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct NoLaunch;
#[async_trait::async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        panic!("initialization must not start a runtime")
    }
}
struct Cut(String);
impl InitProgress for Cut {
    fn reached(&self, stage: InitStage) -> Result<()> {
        if self.0 == "project" && stage == InitStage::ReceiptPublished {
            std::process::exit(86);
        }
        Ok(())
    }
}
impl InstallProgress for Cut {
    fn reached(&self, stage: InstallStage) -> Result<()> {
        let point = match stage {
            InstallStage::Validated => "validated".to_owned(),
            InstallStage::Journaled => "journal".to_owned(),
            InstallStage::Published(i) => format!("file-{i}"),
            InstallStage::OwnershipPublished => "ownership".to_owned(),
            InstallStage::Complete => "complete".to_owned(),
        };
        if self.0 == point {
            std::process::exit(86);
        }
        Ok(())
    }
}
async fn initialize(root: &Path, id: Uuid, cut: Option<&str>) -> Value {
    let mut manager = ProjectManager::new(root.join("state")).unwrap();
    let mut setup = ToolSetup::new(
        manager.home().to_owned(),
        root.join("agentchat"),
        Environment {
            user_config_root: Some(root.join("config")),
            ..Environment::default()
        },
        Some(root.join("bin").into_os_string()),
    );
    if let Some(cut) = cut {
        manager = manager.with_progress(Arc::new(Cut(cut.into())));
        setup = setup.with_progress(Arc::new(Cut(cut.into())));
    }
    let operations = Operations::new(manager, Arc::new(NoLaunch)).with_host_tools(setup);
    let envelope = operations
        .invoke(
            id,
            Target {
                project_path: Some(root.join("workspace")),
                ..Target::default()
            },
            Operation::Initialize {
                auto_read: None,
                delivery_confirmation_timeout_seconds: None,
                default_channels: None,
                scope: Some(Scope::Directory),
                relocate: false,
                new_project: false,
            },
        )
        .await;
    serde_json::to_value(envelope).unwrap()
}

#[tokio::test]
#[ignore = "subprocess helper; parent executes all project/setup durable cuts"]
async fn initialization_installation_child() {
    assert_eq!(
        std::env::var_os("HOME"),
        std::env::var_os("AGENTCHAT_EXPECTED_HOME")
    );
    assert_eq!(
        std::env::var_os("CODEX_HOME"),
        std::env::var_os("AGENTCHAT_EXPECTED_CODEX_HOME")
    );
    let root = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_TEST_SETUP_ROOT").unwrap());
    let id = std::env::var("AGENTCHAT_TEST_SETUP_ID")
        .unwrap()
        .parse()
        .unwrap();
    let cut = std::env::var("AGENTCHAT_TEST_SETUP_CUT").unwrap();
    let result = initialize(&root, id, Some(&cut)).await;
    panic!("expected process exit at {cut}: {result}");
}

#[tokio::test]
async fn seven_real_process_cuts_resume_one_project_and_one_owned_tool_installation() {
    for cut in [
        "project",
        "validated",
        "journal",
        "file-0",
        "file-1",
        "ownership",
        "complete",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for directory in ["workspace", "bin", "config"] {
            fs::create_dir(root.join(directory)).unwrap();
        }
        let host = root.join("bin/codex");
        fs::write(&host, "#!/bin/sh\nexit 77\n").unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).unwrap();
        let id = Uuid::new_v4();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "initialization_installation_child"])
            .env("AGENTCHAT_TEST_SETUP_ROOT", root)
            .env("AGENTCHAT_TEST_SETUP_ID", id.to_string())
            .env("AGENTCHAT_TEST_SETUP_CUT", cut)
            .env("AGENTCHAT_HOME", root.join("state"))
            .env("AGENTCHAT_USER_CONFIG_ROOT", root.join("config"))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (source, target) in [
            ("HOME", "AGENTCHAT_EXPECTED_HOME"),
            ("CODEX_HOME", "AGENTCHAT_EXPECTED_CODEX_HOME"),
        ] {
            if let Some(value) = std::env::var_os(source) {
                command.env(target, value);
            } else {
                command.env_remove(target);
            }
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("cut {cut} exceeded guard");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.code(), Some(86), "{cut}");
        assert!(
            !root
                .join(format!("state/integrations/requests/{id}.json"))
                .exists()
        );
        let manager = ProjectManager::new(root.join("state")).unwrap();
        let before = manager
            .discover(root.join("workspace"))
            .await
            .unwrap()
            .unwrap();
        let resumed = initialize(root, id, None).await;
        assert!(resumed.get("error").is_none(), "{cut}: {resumed}");
        assert_eq!(resumed["project_id"], before.identity().id.to_string());
        assert_eq!(resumed["result"]["created"], true);
        assert_eq!(
            resumed["result"]["integrations"]["hosts"]["codex"]["state"],
            "tools_installed"
        );
        assert_eq!(
            resumed["result"]["capabilities"],
            agentchat_hosts::capabilities()
        );
        assert_eq!(initialize(root, id, None).await, resumed);
        let store = manager.open(&before).await.unwrap();
        store.integrity_check().await.unwrap();
        use agentchat_core::ports::Storage;
        let statistics = store.snapshot().await.unwrap().statistics().await.unwrap();
        assert_eq!(statistics.agents, 0);
        assert_eq!(statistics.channels, 1);
        Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
        assert_eq!(
            fs::read_dir(root.join("state/init/requests"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            fs::read_dir(root.join("state/integrations/requests"))
                .unwrap()
                .count(),
            1
        );
        assert!(!root.join("state/init/pending.json").exists());
        assert!(!root.join("state/integrations/pending.json").exists());
    }
}
