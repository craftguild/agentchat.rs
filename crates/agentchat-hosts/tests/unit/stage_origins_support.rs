//! Owned physical subprocess fixtures; no real host settings or model sessions.
use crate::{
    claude::process::{BirthState, ProcessIdentity},
    staging::{OriginReaper, OriginReport, StagePurpose},
};
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
pub(super) const DATA: &[u8] = b"unpublished complete generation";
pub(super) fn env_path(name: &str) -> PathBuf {
    std::env::var_os(name).unwrap().into()
}
pub(super) fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
pub(super) struct Fixture {
    pub root: tempfile::TempDir,
    pub home: PathBuf,
    pub parent: PathBuf,
    pub stage_name: String,
    pub creator: ProcessIdentity,
    child: Child,
}
impl Fixture {
    pub fn start(phase: &str, purpose: StagePurpose) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("state");
        let parent = if phase == "context-published" {
            home.join("contexts")
        } else {
            base.join("public-parent")
        };
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        fs::DirBuilder::new()
            .mode(if phase == "context-published" {
                0o700
            } else {
                0o755
            })
            .create(&parent)
            .unwrap();
        let stage_name = format!(".stage-{}", uuid::Uuid::new_v4());
        let marker = base.join("ready.json");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "staging::tests::writer_child",
                "--nocapture",
            ])
            .env("AGENTCHAT_HOME", &home)
            .env("AGENTCHAT_USER_CONFIG_ROOT", base.join("config"))
            .env_remove("AGENTCHAT_CONTEXT")
            .env("AGENTCHAT_ORIGIN_PARENT", &parent)
            .env("AGENTCHAT_ORIGIN_STAGE", &stage_name)
            .env("AGENTCHAT_ORIGIN_PHASE", phase)
            .env("AGENTCHAT_ORIGIN_MARKER", &marker)
            .env(
                "AGENTCHAT_ORIGIN_PURPOSE",
                serde_json::to_string(&purpose).unwrap(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        wait(|| {
            assert!(child.try_wait().unwrap().is_none());
            marker.exists()
        });
        let creator: ProcessIdentity = serde_json::from_slice(&fs::read(marker).unwrap()).unwrap();
        assert_eq!(creator.pid(), child.id());
        Self {
            root,
            home,
            parent,
            stage_name,
            creator,
            child,
        }
    }
    pub fn stage(&self) -> PathBuf {
        self.parent.join(&self.stage_name)
    }
    pub fn detach_input(&mut self) -> std::process::ChildStdin {
        self.child.stdin.take().unwrap()
    }
    pub fn kill(&mut self) {
        use std::os::unix::process::ExitStatusExt;
        self.child.kill().unwrap();
        assert_eq!(self.child.wait().unwrap().signal(), Some(9));
        assert_eq!(self.creator.birth_state(), BirthState::Gone);
    }
    pub fn collect(&self) -> OriginReport {
        OriginReaper::new(&self.home).step().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
