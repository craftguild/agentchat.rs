//! Real ordinary signal publishers stopped at paired physical cuts.
use super::super::*;
use crate::{
    claude::process::{BirthState, ProcessIdentity},
    staging::OriginReaper,
};
use std::{
    os::unix::process::ExitStatusExt,
    process::{Child, Command, Stdio},
};

pub(crate) fn cut(phase: &str, cwd: &Path, lock: &Path) {
    if std::env::var("AGENTCHAT_SIGNAL_ORIGIN_CUT").ok().as_deref() != Some(phase)
        || std::env::var_os("AGENTCHAT_SIGNAL_TEST_CWD").as_deref() != Some(cwd.as_os_str())
    {
        return;
    }
    let ready = PathBuf::from(std::env::var_os("AGENTCHAT_SIGNAL_TEST_READY").unwrap());
    let temporary = ready.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap();
    file.write_all(&serde_json::to_vec(&serde_json::json!({"phase":phase,"lock":lock})).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    std::fs::rename(temporary, ready).unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
#[ignore = "owned real signal publisher stopped at its selected original cut"]
fn writer_child() {
    let home = PathBuf::from(std::env::var_os("AGENTCHAT_HOME").unwrap());
    let cwd = PathBuf::from(std::env::var_os("AGENTCHAT_SIGNAL_TEST_CWD").unwrap());
    if std::env::var_os("AGENTCHAT_SIGNAL_COLLECTOR").is_some() {
        let mut reaper = OriginReaper::new(&home);
        loop {
            let _ = reaper.step().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if std::env::var_os("AGENTCHAT_SIGNAL_SYNC_FAILURE").is_some() {
        let anchor = anchor_path(&home, &cwd).unwrap();
        let refused = crate::staging::tests::durability::Refusal::at(anchor.parent().unwrap());
        assert!(prepare(&home, &cwd).is_err());
        let lock = anchor.with_extension("lock");
        let raw = std::fs::read(&lock).unwrap();
        assert!(raw.starts_with(b"0\n"));
        let record: serde_json::Value = serde_json::from_slice(&raw[2..]).unwrap();
        let pending = cwd.join(record["staging"].as_str().unwrap());
        let signal_id = (
            record["signal_identity"][0].as_u64().unwrap(),
            record["signal_identity"][1].as_u64().unwrap(),
        );
        assert_eq!(std::fs::read(&pending).unwrap(), b"0\n");
        let original = std::fs::metadata(&pending).unwrap();
        assert_eq!((original.dev(), original.ino()), signal_id);
        drop(refused);
        prepare(&home, &cwd).unwrap();
        let final_signal = std::fs::metadata(cwd.join(".agentchat-wakeup")).unwrap();
        assert_eq!((final_signal.dev(), final_signal.ino()), signal_id);
        assert_eq!(send(&home, &cwd, Ok).unwrap(), 1);
        println!("paired signal sync recovery verified");
        return;
    }
    prepare(&home, &cwd).unwrap();
    panic!("Signal publisher missed its selected cut");
}

#[test]
fn failed_private_lock_sync_preserves_the_paired_signal_for_exact_retry() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    namespace::private(&home).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "claude::signal::tests::origins::writer_child",
            "--nocapture",
        ])
        .env("AGENTCHAT_HOME", &home)
        .env("AGENTCHAT_USER_CONFIG_ROOT", root.path().join("config"))
        .env("AGENTCHAT_SIGNAL_TEST_CWD", cwd.canonicalize().unwrap())
        .env("AGENTCHAT_SIGNAL_SYNC_FAILURE", "1")
        .env_remove("AGENTCHAT_SIGNAL_ORIGIN_CUT")
        .env_remove("AGENTCHAT_SIGNAL_COLLECTOR")
        .env_remove("AGENTCHAT_CONTEXT")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("paired signal sync recovery verified")
    );
}
#[test]
fn killed_pair_collectors_resume_after_each_data_unlink_without_losing_the_checkpoint() {
    for phase in ["signal-removed", "lock-removed"] {
        let f = Fixture::at("paired");
        let ready = f.home.with_extension("collector-ready");
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "claude::signal::tests::origins::writer_child",
                    "--nocapture",
                ])
                .env("AGENTCHAT_HOME", &f.home)
                .env("AGENTCHAT_USER_CONFIG_ROOT", f.home.join("config"))
                .env_remove("AGENTCHAT_CONTEXT")
                .env("AGENTCHAT_SIGNAL_TEST_CWD", &f.cwd)
                .env("AGENTCHAT_SIGNAL_ORIGIN_CUT", phase)
                .env("AGENTCHAT_SIGNAL_TEST_READY", &ready)
                .env("AGENTCHAT_SIGNAL_COLLECTOR", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        wait(|| {
            assert!(child.0.try_wait().unwrap().is_none());
            ready.exists()
        });
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        f.collect();
        assert!(f.stages.iter().all(|p| !p.exists()));
        assert!(!f.lock.exists() && !f.cwd.join(".agentchat-wakeup").exists());
    }
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    lock: PathBuf,
    stages: Vec<PathBuf>,
    origins: Vec<PathBuf>,
}
fn wait(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
impl Fixture {
    fn at(phase: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("state");
        namespace::private(&home).unwrap();
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();
        let ready = base.join("ready.json");
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "claude::signal::tests::origins::writer_child",
                    "--nocapture",
                ])
                .env("AGENTCHAT_HOME", &home)
                .env("AGENTCHAT_USER_CONFIG_ROOT", base.join("config"))
                .env_remove("AGENTCHAT_CONTEXT")
                .env("AGENTCHAT_SIGNAL_TEST_CWD", &cwd)
                .env("AGENTCHAT_SIGNAL_ORIGIN_CUT", phase)
                .env("AGENTCHAT_SIGNAL_TEST_READY", &ready)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        wait(|| {
            assert!(child.0.try_wait().unwrap().is_none());
            ready.exists()
        });
        let creator = ProcessIdentity::capture(child.0.id()).unwrap();
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        assert_eq!(creator.birth_state(), BirthState::Gone);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        let lock = PathBuf::from(value["lock"].as_str().unwrap());
        let origins = std::fs::read_dir(home.join("staging"))
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(origins.len(), 2);
        let stages = [&cwd, lock.parent().unwrap()]
            .into_iter()
            .flat_map(|p| {
                std::fs::read_dir(p).unwrap().filter_map(|p| {
                    let p = p.unwrap();
                    p.file_name()
                        .to_str()
                        .unwrap()
                        .starts_with(if p.path().parent() == Some(cwd.as_path()) {
                            ".agentchat-wakeup-stage-"
                        } else {
                            ".signal-lock-"
                        })
                        .then(|| p.path())
                })
            })
            .collect();
        Self {
            _root: root,
            home,
            cwd,
            lock,
            stages,
            origins,
        }
    }
    fn collect(&self) {
        let mut reaper = OriginReaper::new(&self.home);
        wait(|| {
            let page = reaper.step().unwrap();
            assert!(page.visited <= 8 && page.removed <= 16);
            self.origins.iter().all(|p| !p.exists())
        });
    }
    fn preserve(&self) {
        let mut reaper = OriginReaper::new(&self.home);
        for _ in 0..6 {
            let page = reaper.step().unwrap();
            assert_eq!(page.removed, 0);
        }
        assert!(self.origins.iter().all(|p| p.exists()));
    }
}
#[test]
fn killed_unpublished_pair_retires_signal_before_lock_and_preserves_foreign_files() {
    let f = Fixture::at("paired");
    let foreign = f.cwd.join(".agentchat-wakeup-stage-user");
    std::fs::write(&foreign, b"foreign").unwrap();
    f.collect();
    assert!(f.stages.iter().all(|p| !p.exists()));
    assert!(!f.lock.exists() && !f.cwd.join(".agentchat-wakeup").exists());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign");
    prepare(&f.home, &f.cwd).unwrap();
    assert_eq!(send(&f.home, &f.cwd, Ok).unwrap(), 1);
}
#[test]
fn winning_pending_genesis_keeps_original_stage_until_an_ordinary_retry() {
    let f = Fixture::at("lock-published");
    let stage = f
        .stages
        .iter()
        .find(|p| p.parent() == Some(f.cwd.as_path()))
        .unwrap();
    let id = std::fs::metadata(stage).unwrap().ino();
    let raw = std::fs::read(&f.lock).unwrap();
    f.preserve();
    assert!(!f.cwd.join(".agentchat-wakeup").exists());
    assert_eq!(std::fs::read(stage).unwrap(), b"0\n");
    assert_eq!(std::fs::read(&f.lock).unwrap(), raw);
    prepare(&f.home, &f.cwd).unwrap();
    assert_eq!(
        std::fs::metadata(f.cwd.join(".agentchat-wakeup"))
            .unwrap()
            .ino(),
        id
    );
    assert_eq!(send(&f.home, &f.cwd, Ok).unwrap(), 1);
    let committed = std::fs::read(&f.lock).unwrap();
    f.collect();
    assert_eq!(std::fs::read(&f.lock).unwrap(), committed);
    assert_eq!(
        std::fs::read(f.cwd.join(".agentchat-wakeup")).unwrap(),
        b"1\n"
    );
}
#[test]
fn killed_published_signal_retires_only_origins_without_creating_an_anchor_or_advancing_a_floor() {
    for phase in ["watched", "anchor"] {
        let f = Fixture::at(phase);
        let anchor = recovery::location(&f.home, &f.cwd);
        let before = std::fs::read(&anchor).ok();
        let raw = std::fs::read(&f.lock).unwrap();
        let signal = f.cwd.join(".agentchat-wakeup");
        let id = std::fs::metadata(&signal).unwrap().ino();
        f.collect();
        assert_eq!(std::fs::read(&anchor).ok(), before);
        assert_eq!(std::fs::read(&f.lock).unwrap(), raw);
        assert_eq!(std::fs::read(&signal).unwrap(), b"0\n");
        assert_eq!(std::fs::metadata(&signal).unwrap().ino(), id);
        prepare(&f.home, &f.cwd).unwrap();
        assert_eq!(send(&f.home, &f.cwd, Ok).unwrap(), 1);
    }
}
#[test]
fn changed_aliased_missing_companions_and_replaced_cwds_keep_complete_pair_proof() {
    for mode in [
        "edit",
        "alias",
        "lock-edit",
        "lock-missing",
        "index-missing",
        "cwd-replaced",
    ] {
        let f = Fixture::at("paired");
        let signal = f
            .stages
            .iter()
            .find(|p| p.parent() == Some(f.cwd.as_path()))
            .unwrap();
        let lock = f
            .stages
            .iter()
            .find(|p| p.parent() != Some(f.cwd.as_path()))
            .unwrap();
        match mode {
            "edit" => std::fs::write(signal, b"1\n").unwrap(),
            "alias" => std::fs::hard_link(signal, f.cwd.join("alias")).unwrap(),
            "lock-edit" => std::fs::write(lock, b"changed").unwrap(),
            "lock-missing" => std::fs::remove_file(lock).unwrap(),
            "index-missing" => {
                let entry = f
                    .origins
                    .iter()
                    .find(|p| {
                        let value: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
                        value["intent"]["purpose"] == "signal_lock"
                    })
                    .unwrap();
                std::fs::remove_file(entry).unwrap();
                let mut reaper = OriginReaper::new(&f.home);
                for _ in 0..4 {
                    assert_eq!(reaper.step().unwrap().removed, 0);
                }
                assert!(signal.exists() && lock.exists());
                continue;
            }
            "cwd-replaced" => {
                std::fs::rename(&f.cwd, f.cwd.with_extension("original")).unwrap();
                std::fs::create_dir(&f.cwd).unwrap();
            }
            _ => unreachable!(),
        }
        f.preserve();
    }
}
