//! Actual dead-writer stages must not indefinitely pin an original dead namespace.
use crate::claude::{
    namespace::{OwnerNamespace, OwnerReaper},
    process::{BirthState, ProcessIdentity},
};
use std::{
    ffi::OsStr,
    fs,
    io::Write,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub(in crate::claude::metadata::staged) fn cut(parent: &Path, stage: &OsStr, target: &OsStr) {
    if target != "owner-state.json"
        || std::env::var_os("AGENTCHAT_STAGE_RETENTION_ROOT").as_deref() != Some(parent.as_os_str())
    {
        return;
    }
    let marker = PathBuf::from(std::env::var_os("AGENTCHAT_STAGE_RETENTION_MARKER").unwrap());
    let metadata = fs::symlink_metadata(parent.join(stage)).unwrap();
    let data = serde_json::to_vec(&serde_json::json!({"stage":stage.to_str().unwrap(),
        "directory":parent,"device":metadata.dev(),"inode":metadata.ino(),"length":metadata.len()}))
    .unwrap();
    let temporary = marker.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap();
    file.write_all(&data).unwrap();
    file.sync_all().unwrap();
    fs::rename(temporary, marker).unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
#[tokio::test]
#[ignore = "owned header writer killed after its complete stage is synchronized"]
async fn writer_child() {
    let home = PathBuf::from(std::env::var_os("AGENTCHAT_HOME").unwrap());
    let owner =
        serde_json::from_str(&std::env::var("AGENTCHAT_STAGE_RETENTION_OWNER").unwrap()).unwrap();
    let ns = OwnerNamespace::existing(&home, owner).await.unwrap();
    let project = PathBuf::from(std::env::var_os("AGENTCHAT_STAGE_RETENTION_PROJECT").unwrap());
    ns.reserve(Uuid::new_v4(), project, None).await.unwrap();
    panic!("Owned writer missed its selected prepared boundary");
}
#[tokio::test]
async fn an_actual_killed_complete_header_writer_does_not_pin_dead_owner_collection() {
    let base = tempfile::tempdir().unwrap();
    let base_path = base.path().canonicalize().unwrap();
    let home = base_path.join("state");
    let project = base_path.join("project");
    fs::create_dir(&project).unwrap();
    let mut native = OwnedChild(
        Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let expected = Path::new("/bin/cat").canonicalize().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let owner = loop {
        if let Ok(token) = ProcessIdentity::capture(native.0.id())
            && token.executable().canonicalize().unwrap() == expected
            && token.is_current()
        {
            break token;
        }
        assert!(native.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let ns = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
    let original = fs::read(ns.directory().join("owner-state.json")).unwrap();
    assert_eq!(ns.generation_floor().await.unwrap(), 0);
    let marker = base_path.join("prepared.json");
    let mut writer = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "claude::metadata::staged::tests::retention::writer_child",
                "--nocapture",
            ])
            .env("AGENTCHAT_HOME", &home)
            .env("AGENTCHAT_USER_CONFIG_ROOT", base_path.join("config"))
            .env("AGENTCHAT_STAGE_RETENTION_ROOT", ns.directory())
            .env("AGENTCHAT_STAGE_RETENTION_MARKER", &marker)
            .env(
                "AGENTCHAT_STAGE_RETENTION_OWNER",
                serde_json::to_string(&owner).unwrap(),
            )
            .env("AGENTCHAT_STAGE_RETENTION_PROJECT", &project)
            .env_remove("AGENTCHAT_CONTEXT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !marker.exists() {
        assert!(writer.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    let stage = value["stage"].as_str().unwrap();
    let token = ProcessIdentity::capture(writer.0.id()).unwrap();
    assert_ne!(token, owner);
    writer.0.kill().unwrap();
    assert_eq!(writer.0.wait().unwrap().signal(), Some(9));
    assert_eq!(token.birth_state(), BirthState::Gone);
    assert!(owner.is_current());
    assert_eq!(
        fs::read(ns.directory().join("owner-state.json")).unwrap(),
        original
    );
    assert_eq!(
        ns.generation_floor().await.unwrap(),
        0,
        "unpublished generation cannot become current"
    );
    let prepared = fs::read(ns.directory().join(stage)).unwrap();
    assert_eq!(prepared.len() as u64, value["length"].as_u64().unwrap());
    native.0.kill().unwrap();
    assert_eq!(native.0.wait().unwrap().signal(), Some(9));
    assert_eq!(owner.birth_state(), BirthState::Gone);
    let quarantine = ns.directory().parent().unwrap().join(format!(
        ".retired-{}",
        ns.directory().file_name().unwrap().to_str().unwrap()
    ));
    let mut reaper = OwnerReaper::new(&home);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let report = reaper.step().unwrap();
        assert!(report.visited <= 8 && report.removed <= 40 && report.directories_removed <= 1);
        if !quarantine.exists() && !ns.directory().exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Original namespace did not recover: {report:?}"
        );
        // A finite maintenance page may preserve a transient observation. Use
        // the same retained cursor on a later scheduling instant, as production
        // does; a burst of tight-loop passes is not a liveness guarantee.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !quarantine.join(stage).exists() && !ns.directory().join(stage).exists(),
        "Complete dead-writer stage pins the original dead namespace indefinitely"
    );
    assert!(!quarantine.exists() && !ns.directory().exists());
}
