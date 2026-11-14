//! Actual private files and owned processes at signal publication crash cuts.
use crate::device_metadata;
#[path = "signal_origins.rs"]
pub(crate) mod origins;
use super::*;
use std::{os::unix::process::ExitStatusExt, process::Stdio};
use tokio::process::Command;

#[test]
fn device_renumbering_preserves_legacy_and_current_signal_inode_and_floor() {
    for old_format in [true, false] {
        let (_root, home, cwd) = fixture();
        if old_format {
            legacy(&home, &cwd, 7, b"7\n");
        } else {
            for expected in 1..=7 {
                assert_eq!(send(&home, &cwd, Ok).unwrap(), expected);
            }
        }
        let anchor = anchor_path(&home, &cwd).unwrap();
        let signal = cwd.join(".agentchat-wakeup");
        let inode = signal.metadata().unwrap().ino();
        device_metadata::renumber(&home);
        device_metadata::rewrite(&anchor, |value| {
            value["dev"] = serde_json::json!(value["dev"].as_u64().unwrap() ^ 1);
        });
        let lock = anchor.with_extension("lock");
        if lock.exists() {
            let raw = std::fs::read(&lock).unwrap();
            let mut record: serde_json::Value = serde_json::from_slice(&raw[2..]).unwrap();
            for field in ["lock_identity", "signal_identity"] {
                record[field][0] = serde_json::json!(record[field][0].as_u64().unwrap() ^ 1);
            }
            std::fs::write(
                &lock,
                [&raw[..2], &serde_json::to_vec(&record).unwrap()].concat(),
            )
            .unwrap();
        }
        prepare(&home, &cwd).unwrap();
        assert_eq!(signal.metadata().unwrap().ino(), inode);
        assert_eq!(std::fs::read(&signal).unwrap(), b"7\n");
        assert_eq!(send(&home, &cwd, Ok).unwrap(), 8);
        assert_eq!(std::fs::read(&signal).unwrap(), b"8\n");
        assert_eq!(signal.metadata().unwrap().ino(), inode);
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    namespace::private(&home).unwrap();
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    (root, home, cwd)
}
fn legacy(home: &Path, cwd: &Path, floor: u64, bytes: &[u8]) -> (u64, u64) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(cwd.join(".agentchat-wakeup"))
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
    let m = file.metadata().unwrap();
    metadata::create(
        &anchor_path(home, cwd).unwrap(),
        &serde_json::json!({"version":1,"cwd":cwd,"dev":m.dev(),"ino":m.ino(),"floor":floor}),
    )
    .unwrap();
    (m.dev(), m.ino())
}
#[test]
fn legacy_interrupted_counter_growth_recovers_the_owned_inode_and_durable_floor() {
    let (_root, home, cwd) = fixture();
    // The old protocol syncs floor 10, then overwrites "9\n" with "10\n".
    // A short write of its first two bytes leaves this exact valid crash prefix.
    let inode = legacy(&home, &cwd, 10, b"10");
    prepare(&home, &cwd).unwrap();
    let signal = cwd.join(".agentchat-wakeup");
    let m = std::fs::metadata(&signal).unwrap();
    assert_eq!((m.dev(), m.ino()), inode);
    assert_eq!(std::fs::read(&signal).unwrap(), b"10\n");
    assert_eq!(send(&home, &cwd, Ok).unwrap(), 11);
    assert_eq!(std::fs::read(&signal).unwrap(), b"11\n");
}

fn child(home: &Path, cwd: &Path, phase: &str, ready: &Path, config: &Path) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "claude::signal::tests::signal_process_child",
        "--ignored",
        "--nocapture",
    ])
    .env("AGENTCHAT_SIGNAL_TEST_HOME", home)
    .env("AGENTCHAT_SIGNAL_TEST_CWD", cwd)
    .env("AGENTCHAT_SIGNAL_TEST_PHASE", phase)
    .env("AGENTCHAT_SIGNAL_TEST_READY", ready)
    .env("AGENTCHAT_HOME", home)
    .env("AGENTCHAT_USER_CONFIG_ROOT", config)
    .env_remove("AGENTCHAT_CONTEXT")
    .current_dir("/")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    cmd
}
fn signal_anchor(file: &File, cwd: &Path) -> Anchor {
    let m = file.metadata().unwrap();
    Anchor {
        version: 2,
        cwd: cwd.into(),
        dev: m.dev(),
        ino: m.ino(),
        floor: 0,
        pending: None,
    }
}
#[test]
#[ignore = "owned subprocess fixture; parents invoke crash cuts and concurrent writers"]
fn signal_process_child() {
    let home = PathBuf::from(std::env::var_os("AGENTCHAT_SIGNAL_TEST_HOME").unwrap());
    let cwd = PathBuf::from(std::env::var_os("AGENTCHAT_SIGNAL_TEST_CWD").unwrap());
    let phase = std::env::var("AGENTCHAT_SIGNAL_TEST_PHASE").unwrap();
    let ready = PathBuf::from(std::env::var_os("AGENTCHAT_SIGNAL_TEST_READY").unwrap());
    if phase == "send" {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut attempts = 0;
        let generation = loop {
            let mut callback_ran = false;
            match send(&home, &cwd, |generation| {
                callback_ran = true;
                Ok(generation)
            }) {
                Ok(generation) => break generation,
                Err(error) if error.code == ErrorCode::Busy && error.retryable => {
                    assert!(
                        !callback_ran,
                        "a rejected attempt cannot repeat its callback"
                    );
                    if attempts >= 31 || Instant::now() >= deadline {
                        // A full parallel suite can exhaust this caller's burst
                        // budget. Report explicit non-acceptance; the parent
                        // retries only these pure, never-invoked callbacks after
                        // all concurrent writers have returned. Timing admission
                        // remains a separate workload gate with unchanged bounds.
                        std::fs::write(&ready, "busy-before-callback").unwrap();
                        return;
                    }
                    // The unchanged product lock is bounded. This pure callback
                    // has no side effect before acquisition; only that documented
                    // retryable contention may be retried by the test caller.
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!("signal writer failed: {error:?}"),
            }
        };
        std::fs::write(ready, generation.to_string()).unwrap();
        return;
    }
    if phase == "callback" {
        let _: Result<()> = send(&home, &cwd, |generation| {
            std::fs::write(&ready, generation.to_string()).unwrap();
            loop {
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        unreachable!();
    }
    let mut held_signal = None;
    let mut held_birth = None;
    let mut held_counter = None;
    if ["intent", "signal", "anchor", "seal"].contains(&phase.as_str()) {
        let anchor = anchor_path(&home, &cwd).unwrap();
        let mut birth = genesis::Birth::open(&home, &anchor, &cwd).unwrap();
        if phase != "intent" {
            let path = cwd.join(".agentchat-wakeup");
            let mut file = birth.signal(&path, &anchor).unwrap();
            lock(&file).unwrap();
            if phase == "anchor" {
                metadata::create(&anchor, &signal_anchor(&file, &cwd)).unwrap();
            }
            if phase == "seal" {
                birth.finish(&mut file, &path, &anchor).unwrap();
            }
            held_signal = Some(file);
        }
        held_birth = Some(birth);
    } else {
        let mut signal = Locked::open(&home, &cwd).unwrap();
        assert_eq!(signal.count, 9);
        signal.reserve(10).unwrap();
        if phase == "partial" || phase == "counter" {
            signal.file.seek(SeekFrom::Start(0)).unwrap();
            signal
                .file
                .write_all(if phase == "partial" { b"10" } else { b"10\n" })
                .unwrap();
            signal.file.sync_all().unwrap();
        }
        if phase == "complete" {
            signal.complete().unwrap();
        }
        held_counter = Some(signal);
    }
    std::fs::write(ready, phase).unwrap();
    // These exact guards stay live, including the real private file lock.
    let _held = (held_signal, held_birth, held_counter);
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}
#[tokio::test]
async fn killed_first_creators_and_counter_writers_recover_without_inode_or_floor_reset() {
    for phase in [
        "intent", "signal", "anchor", "seal", "reserve", "callback", "partial", "counter",
        "complete",
    ] {
        let (root, home, cwd) = fixture();
        let first = ["intent", "signal", "anchor", "seal"].contains(&phase);
        if !first {
            for expected in 1..=9 {
                assert_eq!(send(&home, &cwd, Ok).unwrap(), expected);
            }
        }
        let config = root.path().join("config");
        std::fs::create_dir(&config).unwrap();
        let ready = root.path().join("ready");
        let mut process = child(&home, &cwd, phase, &ready, &config).spawn().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                assert!(
                    process.try_wait().unwrap().is_none(),
                    "{phase} exited before cut"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let anchor = anchor_path(&home, &cwd).unwrap();
        let lock_path = anchor.with_extension("lock");
        let raw = std::fs::read(&lock_path).unwrap();
        let record: serde_json::Value = serde_json::from_slice(&raw[2..]).unwrap();
        let expected_inode = (
            record["signal_identity"][0].as_u64().unwrap(),
            record["signal_identity"][1].as_u64().unwrap(),
        );
        let lock_inode = std::fs::metadata(&lock_path).unwrap();
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert_eq!(
            probe.try_lock_exclusive().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        if phase == "callback" {
            assert_eq!(std::fs::read_to_string(&ready).unwrap(), "10");
        }
        process.kill().await.unwrap();
        let output = process.wait_with_output().await.unwrap();
        assert_eq!(output.status.signal(), Some(9), "{phase}");
        prepare(&home, &cwd).unwrap();
        let m = std::fs::metadata(cwd.join(".agentchat-wakeup")).unwrap();
        assert_eq!((m.dev(), m.ino()), expected_inode, "{phase}");
        let after = std::fs::metadata(&lock_path).unwrap();
        assert_eq!(
            (after.dev(), after.ino()),
            (lock_inode.dev(), lock_inode.ino())
        );
        assert_eq!(std::fs::read(&lock_path).unwrap()[0], b'1');
        let floor = if first { 0 } else { 10 };
        assert_eq!(
            std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
            format!("{floor}\n")
        );
        let state: Anchor = metadata::read(&anchor).unwrap();
        assert_eq!(state.floor, floor);
        assert!(state.pending.is_none());
        assert_eq!(send(&home, &cwd, Ok).unwrap(), floor + 1);
    }
}
#[tokio::test]
async fn exhausted_burst_reports_only_nonaccepted_callbacks_before_parent_retry() {
    let (root, home, cwd) = fixture();
    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let held = Locked::open(&home, &cwd).unwrap();
    let ready = root.path().join("rejected");
    let process = child(&home, &cwd, "send", &ready, &config).spawn().unwrap();
    let output = tokio::time::timeout(Duration::from_secs(12), process.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(ready).unwrap(),
        "busy-before-callback"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "0\n"
    );
    assert_eq!(
        metadata::read::<Anchor>(&anchor_path(&home, &cwd).unwrap())
            .unwrap()
            .floor,
        0
    );
    drop(held);
    assert_eq!(send(&home, &cwd, Ok).unwrap(), 1);
}

#[tokio::test]
async fn separate_first_owners_serialize_one_inode_and_unique_generations() {
    let (root, home, cwd) = fixture();
    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let mut processes = vec![];
    for i in 0..16 {
        let ready = root.path().join(format!("result-{i}"));
        processes.push((
            child(&home, &cwd, "send", &ready, &config).spawn().unwrap(),
            ready,
        ));
    }
    let mut values = vec![];
    let mut rejected = 0;
    for (process, path) in processes {
        let output = tokio::time::timeout(Duration::from_secs(10), process.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result = std::fs::read_to_string(path).unwrap();
        if result == "busy-before-callback" {
            rejected += 1;
        } else {
            values.push(result.parse::<u64>().unwrap());
        }
    }
    assert!(
        !values.is_empty(),
        "concurrent first creators made no progress"
    );
    for _ in 0..rejected {
        // No callback/result from an accepted or uncertain attempt is replayed.
        // Every one of the sixteen requests still must obtain its unique value.
        values.push(send(&home, &cwd, Ok).unwrap());
    }
    values.sort();
    assert_eq!(values, (1..=16).collect::<Vec<_>>());
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "16\n"
    );
    let signal = Locked::open(&home, &cwd).unwrap();
    assert_eq!(signal.count, 16);
    assert_eq!(signal.file.metadata().unwrap().nlink(), 1);
}
#[test]
fn every_owned_short_write_prefix_recovers_and_exhausted_counters_never_wrap() {
    for from in [0, 8, 9, 10, 98, 99, 100, 998, 999, MAX - 1] {
        let to = from + 1;
        let next = format!("{to}\n");
        for end in 0..=next.len() {
            let (_root, home, cwd) = fixture();
            let old = format!("{from}\n");
            legacy(&home, &cwd, from, old.as_bytes());
            let mut signal = Locked::open(&home, &cwd).unwrap();
            signal.reserve(to).unwrap();
            signal.file.seek(SeekFrom::Start(0)).unwrap();
            signal.file.write_all(&next.as_bytes()[..end]).unwrap();
            signal.file.sync_all().unwrap();
            drop(signal);
            prepare(&home, &cwd).unwrap();
            assert_eq!(
                std::fs::read(cwd.join(".agentchat-wakeup")).unwrap(),
                next.as_bytes()
            );
            if to == MAX {
                let before = std::fs::read(anchor_path(&home, &cwd).unwrap()).unwrap();
                assert!(send(&home, &cwd, Ok).is_err());
                assert_eq!(
                    std::fs::read(anchor_path(&home, &cwd).unwrap()).unwrap(),
                    before
                );
            }
        }
    }
}
#[test]
fn failed_callback_consumes_a_durable_generation_before_any_public_counter_change() {
    let (_root, home, cwd) = fixture();
    let result: Result<()> = send(&home, &cwd, |generation| {
        assert_eq!(generation, 1);
        let path = anchor_path(&home, &cwd).unwrap();
        let anchor: Anchor = metadata::read(&path).unwrap();
        assert_eq!(anchor.floor, 1);
        assert_eq!(anchor.pending, Some(counter::Pending { from: 0, to: 1 }));
        assert_eq!(std::fs::read(path.with_extension("lock")).unwrap()[0], b'1');
        assert_eq!(
            std::fs::read(cwd.join(".agentchat-wakeup")).unwrap(),
            b"0\n"
        );
        Err(required())
    });
    assert!(result.is_err());
    prepare(&home, &cwd).unwrap();
    assert_eq!(send(&home, &cwd, Ok).unwrap(), 2);
}
#[test]
fn unanchored_foreign_symlink_and_aliased_signal_files_are_preserved() {
    for kind in [
        "foreign",
        "number",
        "symlink",
        "dangling",
        "hardlink",
        "directory",
    ] {
        let (root, home, cwd) = fixture();
        let path = cwd.join(".agentchat-wakeup");
        let target = root.path().join("foreign-target");
        std::fs::write(&target, b"sentinel").unwrap();
        match kind {
            "foreign" => std::fs::write(&path, b"foreign sentinel").unwrap(),
            "number" => std::fs::write(&path, b"0\n").unwrap(),
            "symlink" => std::os::unix::fs::symlink(&target, &path).unwrap(),
            "dangling" => std::os::unix::fs::symlink(root.path().join("missing"), &path).unwrap(),
            "hardlink" => std::fs::hard_link(&target, &path).unwrap(),
            "directory" => std::fs::create_dir(&path).unwrap(),
            _ => unreachable!(),
        }
        let before = std::fs::symlink_metadata(&path).unwrap();
        assert!(prepare(&home, &cwd).is_err(), "{kind}");
        let after = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(std::fs::read(&target).unwrap(), b"sentinel");
        if kind == "foreign" {
            assert_eq!(std::fs::read(&path).unwrap(), b"foreign sentinel");
        }
        if kind == "number" {
            assert_eq!(std::fs::read(&path).unwrap(), b"0\n");
        }
        assert!(!anchor_path(&home, &cwd).unwrap().exists());
    }
}
#[test]
fn missing_established_records_and_rolled_back_seals_never_recreate_or_reset() {
    for missing in [
        vec!["anchor"],
        vec!["signal"],
        vec!["lock"],
        vec!["anchor", "signal"],
        vec!["anchor", "lock"],
    ] {
        let (_root, home, cwd) = fixture();
        assert_eq!(send(&home, &cwd, Ok).unwrap(), 1);
        let anchor = anchor_path(&home, &cwd).unwrap();
        let paths = [
            ("anchor", anchor.clone()),
            ("lock", anchor.with_extension("lock")),
            ("signal", cwd.join(".agentchat-wakeup")),
        ];
        let originals = paths
            .iter()
            .map(|(name, p)| (*name, std::fs::read(p).unwrap()))
            .collect::<Vec<_>>();
        for (name, p) in &paths {
            if missing.contains(name) {
                std::fs::remove_file(p).unwrap();
            }
        }
        assert!(prepare(&home, &cwd).is_err(), "{missing:?}");
        for ((name, p), (_, bytes)) in paths.iter().zip(&originals) {
            if missing.contains(name) {
                assert!(!p.exists());
            } else {
                assert_eq!(std::fs::read(p).unwrap(), *bytes);
            }
        }
    }
    for seal in [b'0', b'x'] {
        let (_root, home, cwd) = fixture();
        assert_eq!(send(&home, &cwd, Ok).unwrap(), 1);
        let anchor = anchor_path(&home, &cwd).unwrap();
        let lock = anchor.with_extension("lock");
        let mut raw = std::fs::read(&lock).unwrap();
        raw[0] = seal;
        std::fs::write(&lock, &raw).unwrap();
        let before = std::fs::read(&anchor).unwrap();
        assert!(prepare(&home, &cwd).is_err());
        assert_eq!(std::fs::read(&anchor).unwrap(), before);
        assert_eq!(std::fs::read(&lock).unwrap(), raw);
        assert_eq!(
            std::fs::read(cwd.join(".agentchat-wakeup")).unwrap(),
            b"1\n"
        );
    }
}
#[test]
fn malformed_counter_bytes_require_a_matching_owned_pending_write() {
    for pending in [false, true] {
        let (_root, home, cwd) = fixture();
        assert_eq!(send(&home, &cwd, Ok).unwrap(), 1);
        if pending {
            let mut signal = Locked::open(&home, &cwd).unwrap();
            signal.reserve(2).unwrap();
        }
        let path = cwd.join(".agentchat-wakeup");
        std::fs::write(&path, b"foreign").unwrap();
        let anchor = anchor_path(&home, &cwd).unwrap();
        let before = std::fs::read(&anchor).unwrap();
        assert!(prepare(&home, &cwd).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign");
        assert_eq!(std::fs::read(&anchor).unwrap(), before);
    }
}

#[test]
fn declared_first_staging_cannot_adopt_a_foreign_canonical_or_replaced_stage() {
    for replace_stage in [false, true] {
        let (_root, home, cwd) = fixture();
        let anchor = anchor_path(&home, &cwd).unwrap();
        let birth = genesis::Birth::open(&home, &anchor, &cwd).unwrap();
        let raw = std::fs::read(anchor.with_extension("lock")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&raw[2..]).unwrap();
        let stage = cwd.join(value["staging"].as_str().unwrap());
        let stage_inode = std::fs::metadata(&stage).unwrap();
        drop(birth);
        let target = if replace_stage {
            stage.clone()
        } else {
            cwd.join(".agentchat-wakeup")
        };
        let replacement = cwd.join("foreign-replacement");
        std::fs::write(&replacement, b"foreign sentinel").unwrap();
        std::fs::rename(&replacement, &target).unwrap();
        let before = std::fs::symlink_metadata(&target).unwrap();
        assert!(prepare(&home, &cwd).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"foreign sentinel");
        let after = std::fs::symlink_metadata(&target).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert!(!anchor.exists());
        if replace_stage {
            assert!(!cwd.join(".agentchat-wakeup").exists());
        } else {
            let after = std::fs::metadata(&stage).unwrap();
            assert_eq!(
                (stage_inode.dev(), stage_inode.ino()),
                (after.dev(), after.ino())
            );
            assert_eq!(std::fs::read(&stage).unwrap(), b"0\n");
        }
    }
}
#[test]
fn replacing_the_private_lock_or_watched_inode_preserves_all_established_bytes() {
    for replace_lock in [false, true] {
        let (_root, home, cwd) = fixture();
        assert_eq!(send(&home, &cwd, Ok).unwrap(), 1);
        let anchor = anchor_path(&home, &cwd).unwrap();
        let target = if replace_lock {
            anchor.with_extension("lock")
        } else {
            cwd.join(".agentchat-wakeup")
        };
        let before_anchor = std::fs::read(&anchor).unwrap();
        let bytes = std::fs::read(&target).unwrap();
        let temp = target.with_extension("replacement");
        std::fs::write(&temp, &bytes).unwrap();
        std::fs::rename(&temp, &target).unwrap();
        assert!(prepare(&home, &cwd).is_err());
        assert_eq!(std::fs::read(&anchor).unwrap(), before_anchor);
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
    }
}
#[test]
fn a_changed_signal_during_callback_cannot_commit_over_the_foreign_successor() {
    let (_root, home, cwd) = fixture();
    prepare(&home, &cwd).unwrap();
    let signal = cwd.join(".agentchat-wakeup");
    let old = cwd.join("retained-original");
    let result = send(&home, &cwd, |generation| {
        assert_eq!(generation, 1);
        std::fs::rename(&signal, &old).unwrap();
        std::fs::write(&signal, b"foreign sentinel").unwrap();
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&signal).unwrap(), b"foreign sentinel");
    assert_eq!(std::fs::read(&old).unwrap(), b"0\n");
    let anchor: Anchor = metadata::read(&anchor_path(&home, &cwd).unwrap()).unwrap();
    assert_eq!(anchor.floor, 1);
    assert_eq!(anchor.pending, Some(counter::Pending { from: 0, to: 1 }));
}
