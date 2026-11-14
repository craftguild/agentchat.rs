//! Disposable project/config roots and Git fixtures; no developer config writes.

#![allow(dead_code)]

pub mod native_host;
pub mod runtime;

use agentchat_runtime::project::{InitRequest, ProjectManager};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;
use uuid::Uuid;

pub struct SyntheticHost;
#[async_trait::async_trait]
impl agentchat_core::ports::HostGate for SyntheticHost {
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::RetainUncertainty,
            ActiveInput::ExactTurnId,
        ))
    }
    fn allows_unmanaged_participation(&self) -> bool {
        true
    }
    async fn verify(
        &self,
        context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::ports::HostState> {
        if context.host_version != "fixture-only" || context.os_profile != "synthetic-not-admitted"
        {
            return Err(agentchat_core::Error::new(
                agentchat_core::ErrorCode::UnsupportedHost,
                "No real host profile is admitted by this fixture.",
            ));
        }
        Ok(agentchat_core::ports::HostState {
            phase: agentchat_core::model::Phase::Idle,
            active_turn_id: None,
            wake_paused: false,
        })
    }
}

pub struct Fixture {
    pub root: TempDir,
    pub workspace: PathBuf,
    pub config: PathBuf,
    pub hooks: PathBuf,
    pub manager: ProjectManager,
}
impl Fixture {
    pub fn new() -> Self {
        let root = TempDir::new().unwrap();
        Self::from_root(root)
    }
    pub fn new_in(parent: &Path) -> Self {
        Self::from_root(TempDir::new_in(parent).unwrap())
    }
    fn from_root(root: TempDir) -> Self {
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let config = root.path().join("config");
        fs::create_dir(&config).unwrap();
        let hooks = root.path().join("empty-hooks");
        fs::create_dir(&hooks).unwrap();
        let manager = ProjectManager::new(root.path().join("state")).unwrap();
        Self {
            root,
            workspace,
            config,
            hooks,
            manager,
        }
    }
    pub fn git(&self, path: &Path, args: &[&str]) -> Vec<u8> {
        let mut command = Command::new("git");
        command
            .arg("-c")
            .arg(format!("core.hooksPath={}", self.hooks.display()))
            .arg("-c")
            .arg(format!("init.templateDir={}", self.hooks.display()))
            .args([
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=AGENTCHAT Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .arg("-C")
            .arg(path)
            .args(args)
            .env("LC_ALL", "C");
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_PREFIX",
        ] {
            command.env_remove(key);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "Git fixture {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    pub fn repository(&self, path: &Path) {
        fs::create_dir_all(path).unwrap();
        self.git(path, &["init", "--quiet", "--initial-branch=fixture"]);
        fs::create_dir_all(path.join(".git/info")).unwrap();
        // Fixture-only commands suppress inherited hooks/signing, avoiding
        // developer automation. Production discovery preserves Git controls.
        self.git(
            path,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "disposable fixture",
            ],
        );
    }
    pub fn request(&self) -> InitRequest {
        InitRequest::new(Uuid::new_v4())
    }
}
