//! Real kernel-owner observations and owned helper lifetime fences.
use agentchat_hosts::claude::process::ProcessIdentity;
use serde_json::{Value, json};
use std::{
    io::Read,
    os::unix::{fs::OpenOptionsExt, fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

fn private() -> TempDir {
    tempfile::Builder::new()
        .prefix("agentchat-native-owner-test-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn launch(root: &Path, mode: &str, expected: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "native_owner_child", "--nocapture"])
        .env("AGENTCHAT_OWNER_TEST_ROOT", root)
        .env("AGENTCHAT_OWNER_TEST_MODE", mode)
        .env("AGENTCHAT_OWNER_TEST_EXPECTED", expected)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}
fn report(root: &Path, child: &mut Child) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(bytes) = std::fs::read(root.join("result.json")) {
            return serde_json::from_slice(&bytes).unwrap();
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "owned helper exited before reporting"
        );
        assert!(Instant::now() < deadline, "owned helper report timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn wait_offline(identity: &ProcessIdentity) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while identity.is_current() {
        assert!(Instant::now() < deadline, "exited owner remains live");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn changed(mut value: Value, field: &str, replacement: Value) -> bool {
    value[field] = replacement;
    serde_json::from_value::<ProcessIdentity>(value)
        .unwrap()
        .is_current()
}

#[test]
fn exact_kernel_identity_roundtrips_and_every_fence_matters() {
    let token = ProcessIdentity::capture(std::process::id()).unwrap();
    assert!(token.is_current());
    assert_eq!(
        token.executable().canonicalize().unwrap(),
        std::env::current_exe().unwrap().canonicalize().unwrap()
    );
    let value = serde_json::to_value(&token).unwrap();
    assert!(
        serde_json::from_value::<ProcessIdentity>(value.clone())
            .unwrap()
            .is_current()
    );
    assert!(!changed(value.clone(), "boot", json!(Uuid::new_v4())));
    assert!(!changed(
        value.clone(),
        "uid",
        json!(value["uid"].as_u64().unwrap() ^ 1)
    ));
    for pid in [0, u32::MAX] {
        assert!(!changed(value.clone(), "pid", json!(pid)));
        assert!(ProcessIdentity::capture(pid).is_err());
    }
    let mut start = value["start"].clone();
    let field = if cfg!(target_os = "linux") {
        "ticks"
    } else {
        "microseconds"
    };
    start[field] = json!(start[field].as_u64().unwrap() + 1);
    assert!(!changed(value.clone(), "start", start));
    let mut image = value["image"].clone();
    image["inode"] = json!(image["inode"].as_u64().unwrap() ^ 1);
    assert!(!changed(value.clone(), "image", image));
    for bytes in [Vec::new(), vec![0], vec![b'x'; 16_385]] {
        let mut image = value["image"].clone();
        image["path"] = json!(bytes);
        assert!(!changed(value.clone(), "image", image));
    }
    let mut extra = value;
    extra["guessed_root"] = json!(Uuid::new_v4());
    assert!(serde_json::from_value::<ProcessIdentity>(extra).is_err());
}

#[test]
fn device_renumbering_keeps_initial_image_but_cannot_renew_an_old_boot() {
    let token = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut value = serde_json::to_value(&token).unwrap();
    value["image"]["device"] = json!(value["image"]["device"].as_u64().unwrap() ^ 1);
    let renumbered: ProcessIdentity = serde_json::from_value(value.clone()).unwrap();
    assert!(renumbered.is_current());
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    assert!(renumbered.matches_executable(&executable, executable.metadata().unwrap()));
    assert!(renumbered.is_continuing());
    value["boot"] = json!(Uuid::new_v4());
    let retired: ProcessIdentity = serde_json::from_value(value).unwrap();
    assert!(!retired.is_current());
    assert_eq!(
        retired.continuity_state(),
        agentchat_hosts::claude::process::ContinuityState::Gone
    );
}

#[test]
fn native_parent_capture_requires_the_exact_image_and_survives_only_its_owner() {
    let root = private();
    let executable = std::env::current_exe().unwrap();
    let own = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut child = launch(root.path(), "parent", &executable);
    let value = report(root.path(), &mut child);
    let observed: ProcessIdentity = serde_json::from_value(value["parent"].clone()).unwrap();
    assert_eq!(observed, own);
    assert_ne!(observed.pid(), child.id());
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    assert!(own.is_current());

    let wrong = root.path().join("wrong-image");
    std::fs::write(&wrong, b"not a native executable").unwrap();
    let other = private();
    let mut child = launch(other.path(), "wrong-parent", &wrong);
    let value = report(other.path(), &mut child);
    assert_eq!(value["refused"], true);
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
}

#[test]
fn normal_owner_exit_is_offline_while_the_observing_parent_stays_live() {
    let root = private();
    let mut child = launch(root.path(), "owner", &std::env::current_exe().unwrap());
    let value = report(root.path(), &mut child);
    let identity: ProcessIdentity = serde_json::from_value(value["owner"].clone()).unwrap();
    assert_eq!(identity.pid(), child.id());
    assert!(identity.is_current());
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    assert!(!identity.is_current());
    assert!(
        ProcessIdentity::capture(std::process::id())
            .unwrap()
            .is_current()
    );
}

#[test]
fn sigkill_and_unreaped_zombies_do_not_keep_the_owner_live() {
    let root = private();
    let mut child = launch(root.path(), "owner", &std::env::current_exe().unwrap());
    let value = report(root.path(), &mut child);
    let identity: ProcessIdentity = serde_json::from_value(value["owner"].clone()).unwrap();
    assert!(identity.is_current());
    child.kill().unwrap();
    // Do not reap until the kernel observation has refused the exited owner.
    wait_offline(&identity);
    assert_eq!(child.wait().unwrap().signal(), Some(9));
    assert!(!identity.is_current());
}

#[test]
fn physical_ancestry_skips_a_wrapper_and_does_not_select_a_live_peer() {
    let root = private();
    let peer_root = private();
    let executable = std::env::current_exe().unwrap();
    let own = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut peer = launch(peer_root.path(), "owner", &executable);
    let peer_value = report(peer_root.path(), &mut peer);
    let peer_identity: ProcessIdentity =
        serde_json::from_value(peer_value["owner"].clone()).unwrap();
    let mut wrapper = Command::new("/bin/sh")
        .args([
            "-c",
            "\"$AGENTCHAT_OWNER_TEST_EXE\" --ignored --exact native_owner_child --nocapture & wait \"$!\"",
        ])
        .env("AGENTCHAT_OWNER_TEST_EXE", &executable)
        .env("AGENTCHAT_OWNER_TEST_ROOT", root.path())
        .env("AGENTCHAT_OWNER_TEST_MODE", "ancestor")
        .env("AGENTCHAT_OWNER_TEST_EXPECTED", &executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let value = report(root.path(), &mut wrapper);
    let ancestor: ProcessIdentity = serde_json::from_value(value["ancestor"].clone()).unwrap();
    assert_eq!(ancestor, own);
    assert_ne!(ancestor, peer_identity);
    assert_ne!(ancestor.pid(), wrapper.id());
    assert!(ancestor.is_current() && peer_identity.is_current());
    drop(wrapper.stdin.take());
    assert!(wrapper.wait().unwrap().success());
    drop(peer.stdin.take());
    assert!(peer.wait().unwrap().success());
}

#[test]
fn physical_ancestry_refuses_an_unrelated_executable() {
    let root = private();
    let mut child = launch(root.path(), "wrong-ancestor", Path::new("/usr/bin/true"));
    assert_eq!(report(root.path(), &mut child)["refused"], true);
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    assert!(ProcessIdentity::ancestor(Path::new("/definitely-missing-native-image")).is_err());
}

#[test]
#[ignore = "isolated native process helper explicitly exercised by the parent tests"]
fn native_owner_child() {
    let root = PathBuf::from(std::env::var_os("AGENTCHAT_OWNER_TEST_ROOT").unwrap());
    let expected = PathBuf::from(std::env::var_os("AGENTCHAT_OWNER_TEST_EXPECTED").unwrap());
    let mode = std::env::var("AGENTCHAT_OWNER_TEST_MODE").unwrap();
    let value = match mode.as_str() {
        "parent" => json!({"parent": ProcessIdentity::parent(&expected).unwrap()}),
        "wrong-parent" => json!({"refused":ProcessIdentity::parent(&expected).is_err()}),
        "owner" => json!({"owner":ProcessIdentity::capture(std::process::id()).unwrap()}),
        "ancestor" => json!({"ancestor":ProcessIdentity::ancestor(&expected).unwrap()}),
        "wrong-ancestor" => json!({"refused":ProcessIdentity::ancestor(&expected).is_err()}),
        _ => panic!("unknown isolated helper mode"),
    };
    let path = root.join("result.json");
    let stage = root.join("stage.json");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&stage)
        .unwrap();
    serde_json::to_writer(&mut file, &value).unwrap();
    drop(file);
    std::fs::rename(stage, path).unwrap();
    std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
}
