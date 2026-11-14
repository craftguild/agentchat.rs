//! The real backup runs in an owned process; exits skip all destructors.
use agentchat_core::{
    ErrorCode,
    project::{ProjectIdentity, Scope},
};
use agentchat_store::{BackupProgress, BackupStage, Store};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

fn key(stage: BackupStage) -> String {
    match stage {
        BackupStage::SnapshotAnchored { .. } => "SnapshotAnchored".into(),
        BackupStage::CopyChunk { table, rows } => format!("{table:?}:{rows}"),
        other => format!("{other:?}"),
    }
}
struct Observer {
    root: PathBuf,
    cut: String,
    race: bool,
}
impl Observer {
    fn wait(&self, phase: &str) {
        fs::write(self.root.join(format!("ready-{phase}")), b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.root.join(format!("continue-{phase}")).exists() {
            assert!(Instant::now() < deadline, "parent did not release {phase}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl BackupProgress for Observer {
    fn reached(&self, stage: BackupStage) {
        let name = key(stage);
        let mut log = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(self.root.join("progress.jsonl"))
            .unwrap();
        writeln!(log, "{}", json!({"key":name,"stage":format!("{stage:?}")})).unwrap();
        log.sync_all().unwrap();
        if matches!(stage, BackupStage::SnapshotAnchored { .. }) {
            self.wait("snapshot");
        }
        if self.race && stage == BackupStage::FileSynced {
            self.wait("publish");
        }
        if !self.race && name == self.cut {
            std::process::exit(86);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "subprocess helper; the parent invokes all backup cuts and publication races"]
async fn backup_child() {
    let root = PathBuf::from(std::env::var_os("AGENTCHAT_TEST_BACKUP_ROOT").unwrap());
    let input: Value = serde_json::from_slice(&fs::read(root.join("child.json")).unwrap()).unwrap();
    let identity = ProjectIdentity {
        id: Uuid::parse_str(input["project"].as_str().unwrap()).unwrap(),
        scope: Scope::Directory,
    };
    let store = Store::open(root.join("agentchat.sqlite3"), identity)
        .await
        .unwrap();
    let race = input["race"].as_bool().unwrap();
    let result = store
        .backup_observed(
            root.join("backup.sqlite3"),
            Arc::new(Observer {
                root,
                cut: input["cut"].as_str().unwrap().into(),
                race,
            }),
        )
        .await;
    if race {
        assert_eq!(result.unwrap_err().code, ErrorCode::ConfigurationConflict);
    } else {
        panic!("selected backup cut was not reached: {result:?}");
    }
    store.close().await.unwrap();
}

pub struct Process {
    child: Child,
    deadline: Instant,
}
impl Process {
    pub fn spawn(root: &Path, project: Uuid, cut: &str, race: bool) -> Self {
        fs::write(
            root.join("child.json"),
            serde_json::to_vec(&json!({"project":project,"cut":cut,"race":race})).unwrap(),
        )
        .unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "process::backup_child"])
            .env("AGENTCHAT_TEST_BACKUP_ROOT", root)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Self {
            child,
            deadline: Instant::now() + Duration::from_secs(40),
        }
    }
    pub fn poll(&mut self) -> Option<ExitStatus> {
        assert!(
            Instant::now() < self.deadline,
            "backup child exceeded its deadline"
        );
        self.child.try_wait().unwrap()
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub fn stages(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("progress.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
pub fn staging(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".agentchat-backup-")
        })
        .collect()
}
