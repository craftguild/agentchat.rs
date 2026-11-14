//! Actual owned kernel processes exercise retention observations without host admission.
use agentchat_hosts::claude::process::{BirthState, ContinuityState, ProcessIdentity};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

struct Owner {
    child: Child,
    _directory: TempDir,
    token: ProcessIdentity,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}
fn launch(exec: bool) -> Owner {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "birth_child", "--nocapture"])
        .env("AGENTCHAT_BIRTH_TEST_DIRECTORY", directory.path())
        .env("AGENTCHAT_BIRTH_TEST_EXEC", if exec { "yes" } else { "no" })
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(bytes) = std::fs::read(directory.path().join("token.json")) {
            let token: ProcessIdentity = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(token.pid(), child.id());
            return Owner {
                child,
                _directory: directory,
                token,
            };
        }
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned birth helper failed to report");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn eventually(expected: BirthState, token: &ProcessIdentity) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = token.birth_state();
        if value == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected:?}, observed {value:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn changed(value: &Value, field: &str, replacement: Value) -> ProcessIdentity {
    let mut value = value.clone();
    value[field] = replacement;
    serde_json::from_value(value).unwrap()
}
#[test]
fn observable_original_birth_is_present() {
    let token = ProcessIdentity::capture(std::process::id()).unwrap();
    assert_eq!(token.birth_state(), BirthState::Present);
    assert!(token.is_current());
}
#[test]
fn reversible_image_and_uid_mismatches_do_not_establish_birth_loss() {
    let token = ProcessIdentity::capture(std::process::id()).unwrap();
    let value = serde_json::to_value(&token).unwrap();
    let mut variants = vec![changed(
        &value,
        "uid",
        json!(value["uid"].as_u64().unwrap() ^ 1),
    )];
    let mut image = value["image"].clone();
    image["inode"] = json!(image["inode"].as_u64().unwrap() ^ 1);
    variants.push(changed(&value, "image", image));
    for changed in variants {
        assert!(!changed.is_current());
        assert_eq!(changed.birth_state(), BirthState::Present);
    }
    let mut image = value["image"].clone();
    image["device"] = json!(image["device"].as_u64().unwrap() ^ 1);
    let renumbered = changed(&value, "image", image);
    assert!(renumbered.is_current());
    assert_eq!(renumbered.birth_state(), BirthState::Present);
}
#[test]
fn actual_exec_changes_image_without_ending_the_original_birth() {
    let mut owner = launch(true);
    owner.child.stdin.as_mut().unwrap().write_all(b"e").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let replacement = loop {
        assert!(owner.child.try_wait().unwrap().is_none());
        if let Ok(now) = ProcessIdentity::capture(owner.child.id())
            && now.executable() != owner.token.executable()
        {
            break now;
        }
        assert!(Instant::now() < deadline, "owned child never execed");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(!owner.token.is_current());
    // Accepted ADR0027 boundary: same-birth exec is not detected by continuity.
    assert_eq!(owner.token.continuity_state(), ContinuityState::Current);
    assert_ne!(replacement.executable(), owner.token.executable());
    assert_eq!(owner.token.birth_state(), BirthState::Present);
    assert!(replacement.is_current());
}
#[test]
fn normal_exit_positively_loses_birth_while_the_observer_survives() {
    let mut owner = launch(false);
    assert_eq!(owner.token.birth_state(), BirthState::Present);
    drop(owner.child.stdin.take());
    assert!(owner.child.wait().unwrap().success());
    eventually(BirthState::Gone, &owner.token);
    assert_eq!(
        ProcessIdentity::capture(std::process::id())
            .unwrap()
            .birth_state(),
        BirthState::Present
    );
}
#[test]
fn killed_owner_uncertainty_is_preserved_until_positive_birth_loss() {
    let mut owner = launch(false);
    owner.child.kill().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let unreaped = loop {
        // Full current-owner observation and physical birth are independent.
        // Signal delivery and process teardown can overlap either observation;
        // wait for an explicit birth result instead of inferring it from false.
        let state = owner.token.birth_state();
        #[cfg(target_os = "linux")]
        let ready = state == BirthState::Gone;
        #[cfg(target_os = "macos")]
        let ready = state != BirthState::Present;
        if ready {
            break state;
        }
        assert!(
            Instant::now() < deadline,
            "killed owner's birth remains {state:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    // Some Darwin libproc versions hide unreaped zombie births even though
    // kill(pid,0) still reports existence. That is uncertainty, not GC permission.
    assert_ne!(unreaped, BirthState::Present);
    #[cfg(target_os = "macos")]
    {
        let query =
            libproc::proc_pid::pidinfo::<libproc::bsd_info::BSDInfo>(owner.child.id() as i32, 0);
        let detail = match query {
            Ok(v) => {
                json!({"pid":v.pbi_pid,"state":v.pbi_status,"seconds":v.pbi_start_tvsec,"microseconds":v.pbi_start_tvusec})
            }
            Err(error) => json!({"libproc_error":error}),
        };
        let existence = rustix::process::test_kill_process(
            rustix::process::Pid::from_raw(owner.child.id() as i32).unwrap(),
        );
        eprintln!(
            "owned unreaped birth: {unreaped:?}; {detail}; non-signalling query: {existence:?}"
        );
    }
    #[cfg(target_os = "linux")]
    assert_eq!(unreaped, BirthState::Gone);
    assert_eq!(owner.child.wait().unwrap().signal(), Some(9));
    eventually(BirthState::Gone, &owner.token);
}
#[test]
fn malformed_birth_identifiers_preserve_uncertainty() {
    let token = ProcessIdentity::capture(std::process::id()).unwrap();
    let value = serde_json::to_value(&token).unwrap();
    for pid in [0, u32::MAX] {
        assert_eq!(
            changed(&value, "pid", json!(pid)).birth_state(),
            BirthState::Unknown
        );
    }
    assert_eq!(
        changed(&value, "boot", json!(Uuid::nil())).birth_state(),
        BirthState::Unknown
    );
    #[cfg(target_os = "linux")]
    let invalid = [
        json!({"platform":"linux","ticks":0}),
        json!({"platform":"macos","seconds":1,"microseconds":0}),
    ];
    #[cfg(target_os = "macos")]
    let invalid = [
        json!({"platform":"macos","seconds":1,"microseconds":1_000_000}),
        json!({"platform":"linux","ticks":1}),
    ];
    for start in invalid {
        assert_eq!(
            changed(&value, "start", start).birth_state(),
            BirthState::Unknown
        );
    }
}
#[test]
fn distinct_saved_boot_and_start_cannot_name_the_observed_birth() {
    let token = ProcessIdentity::capture(std::process::id()).unwrap();
    let value = serde_json::to_value(&token).unwrap();
    assert_eq!(
        changed(&value, "boot", json!(Uuid::new_v4())).birth_state(),
        BirthState::Gone
    );
    let mut start = value["start"].clone();
    let field = if cfg!(target_os = "linux") {
        "ticks"
    } else {
        "seconds"
    };
    start[field] = json!(start[field].as_u64().unwrap() + 1);
    assert_eq!(
        changed(&value, "start", start).birth_state(),
        BirthState::Gone
    );
    assert_eq!(token.birth_state(), BirthState::Present);
}
#[test]
#[ignore = "owned subprocess explicitly exercised by the parent tests"]
fn birth_child() {
    let directory =
        std::path::PathBuf::from(std::env::var_os("AGENTCHAT_BIRTH_TEST_DIRECTORY").unwrap());
    let token = ProcessIdentity::capture(std::process::id()).unwrap();
    let stage = directory.join("staging.json");
    std::fs::write(&stage, serde_json::to_vec(&token).unwrap()).unwrap();
    std::fs::rename(stage, directory.join("token.json")).unwrap();
    if std::env::var("AGENTCHAT_BIRTH_TEST_EXEC").unwrap() == "yes" {
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
        panic!(
            "owned exec failed: {}",
            Command::new("/bin/sleep").arg("20").exec()
        );
    }
    std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
}
