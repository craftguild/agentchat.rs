//! Actual SIGKILL cuts around immutable publication, never host/G0 acceptance.
use super::*;
use crate::{context::ContextRegistry, project::ProjectManager};
use agentchat_core::{
    ErrorCode,
    model::{Context, HostKind},
};
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
use std::{
    os::unix::ffi::OsStrExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub(super) fn cut(phase: &str, staged: &Path, target: &Path) {
    let Ok(selected) = std::env::var("AGENTCHAT_PUBLICATION_CUT") else {
        return;
    };
    if std::env::var_os("AGENTCHAT_PUBLICATION_TARGET").as_deref() != Some(target.as_os_str()) {
        return;
    }
    let report =
        std::path::PathBuf::from(std::env::var_os("AGENTCHAT_PUBLICATION_REPORT").unwrap());
    let metadata = fs::symlink_metadata(if phase == "prepared" { staged } else { target }).unwrap();
    let value = json!({"phase":phase,"stage":staged,"target":target,
        "device":metadata.dev(),"inode":metadata.ino(),"links":metadata.nlink()});
    let temporary = report.with_extension(format!("{phase}.tmp"));
    let destination = report.with_extension(format!("{phase}.json"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap();
    file.write_all(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    fs::rename(temporary, destination).unwrap();
    if selected == phase {
        if let Some(resume) = std::env::var_os("AGENTCHAT_PUBLICATION_RESUME") {
            // Only the owned error-path fixture resumes this barrier after
            // replacing a name/parent and forcing an actual NOREPLACE failure.
            while !Path::new(&resume).exists() {
                std::thread::sleep(Duration::from_millis(5));
            }
            return;
        }
        loop {
            std::thread::park();
        }
    }
}

#[test]
#[ignore = "owned writer; parents SIGKILL both actual publication boundaries"]
fn publication_writer_child() {
    assert_eq!(
        std::env::var_os("HOME"),
        std::env::var_os("AGENTCHAT_EXPECTED_HOME")
    );
    assert_eq!(
        std::env::var_os("CODEX_HOME"),
        std::env::var_os("AGENTCHAT_EXPECTED_CODEX_HOME")
    );
    let target =
        std::path::PathBuf::from(std::env::var_os("AGENTCHAT_PUBLICATION_TARGET").unwrap());
    let input = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_PUBLICATION_INPUT").unwrap());
    let value: Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    let home = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_HOME").unwrap());
    let result = write_new(&home, &target, &value, 128 * 1024);
    if std::env::var_os("AGENTCHAT_PUBLICATION_EXPECT_FAILURE").is_some() {
        assert!(result.is_err(), "Owned collision unexpectedly published");
        return;
    }
    result.unwrap();
    panic!("Owned writer missed its selected publication cut");
}

#[path = "context_orphan_recovery.rs"]
mod orphan_recovery;
#[path = "context_stage_cleanup.rs"]
mod stage_cleanup;
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
async fn exercise(phase: &str) {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let home = root.path().canonicalize().unwrap();
    let contexts = home.join("contexts");
    directory(&contexts).unwrap();
    let expected = Context {
        context_id: Uuid::new_v4(),
        incarnation: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        host: HostKind::Claude,
        host_session_id: Uuid::new_v4().to_string(),
        host_version: "fixture-only".into(),
        os_profile: "fixture-only".into(),
        working_directory: home.clone(),
    };
    let target = contexts.join(format!("{}.json", expected.context_id));
    let value = json!({"version":1,"context_id":expected.context_id,
        "incarnation":expected.incarnation,"project_id":expected.project_id,
        "host":expected.host,"host_session_id":expected.host_session_id,
        "host_version":expected.host_version,"os_profile":expected.os_profile,
        "cwd_bytes":expected.working_directory.as_os_str().as_bytes()});
    let input = home.join("input.json");
    fs::write(&input, serde_json::to_vec(&value).unwrap()).unwrap();
    let sentinel = contexts.join(".context-foreign");
    fs::write(&sentinel, b"foreign sentinel").unwrap();
    let sentinel_id = fs::metadata(&sentinel).unwrap().ino();
    let report = home.join("barrier");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "filesystem::tests::publication_writer_child",
        ])
        .env("AGENTCHAT_HOME", &home)
        .env("AGENTCHAT_USER_CONFIG_ROOT", home.join("config"))
        .env("AGENTCHAT_PUBLICATION_CUT", phase)
        .env("AGENTCHAT_PUBLICATION_TARGET", &target)
        .env("AGENTCHAT_PUBLICATION_INPUT", &input)
        .env("AGENTCHAT_PUBLICATION_REPORT", &report)
        .env_remove("AGENTCHAT_CONTEXT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    for (actual, captured) in [
        ("HOME", "AGENTCHAT_EXPECTED_HOME"),
        ("CODEX_HOME", "AGENTCHAT_EXPECTED_CODEX_HOME"),
    ] {
        if let Some(value) = std::env::var_os(actual) {
            command.env(captured, value);
        } else {
            command.env_remove(captured);
        }
    }
    let mut child = OwnedChild(command.spawn().unwrap());
    let barrier = report.with_extension(format!("{phase}.json"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !barrier.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "Owned writer exited before {phase}"
        );
        assert!(
            Instant::now() < deadline,
            "Owned writer did not reach {phase}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let observed: Value = serde_json::from_slice(&fs::read(&barrier).unwrap()).unwrap();
    child.0.kill().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(child.0.wait().unwrap().signal(), Some(9));
    let prepared: Value =
        serde_json::from_slice(&fs::read(report.with_extension("prepared.json")).unwrap()).unwrap();
    let stage = std::path::PathBuf::from(prepared["stage"].as_str().unwrap());
    let registry = ContextRegistry::new(ProjectManager::new(&home).unwrap());
    if phase == "prepared" {
        assert!(!target.exists());
        assert_eq!(
            registry
                .descriptor(expected.context_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::SessionRequired
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&stage).unwrap()).unwrap(),
            value
        );
    } else {
        assert!(!stage.exists());
        assert_eq!(
            registry.descriptor(expected.context_id).await.unwrap(),
            expected
        );
        let metadata = fs::metadata(&target).unwrap();
        assert_eq!(
            (metadata.dev(), metadata.ino()),
            (
                prepared["device"].as_u64().unwrap(),
                prepared["inode"].as_u64().unwrap()
            )
        );
        assert_eq!(metadata.nlink(), 1);
    }
    assert_eq!(observed["links"], 1);
    assert_eq!(fs::metadata(&sentinel).unwrap().ino(), sentinel_id);
    assert_eq!(fs::read(&sentinel).unwrap(), b"foreign sentinel");
    assert_eq!(fs::read_dir(&contexts).unwrap().count(), 2);
}
#[tokio::test]
async fn killed_prepublication_writer_leaves_only_an_unpublished_stage() {
    exercise("prepared").await;
}
#[tokio::test]
async fn killed_published_writer_keeps_one_canonical_inode_without_a_stage_alias() {
    exercise("published").await;
}
