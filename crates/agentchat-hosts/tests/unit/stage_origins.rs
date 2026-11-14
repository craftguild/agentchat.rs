//! Actual physical writers, complete proof, ambiguity and selective recovery.
#[path = "stage_origins_durability.rs"]
pub(crate) mod durability;
#[path = "stage_origins_support.rs"]
mod support;
use super::*;
use crate::claude::{
    metadata::directory::Directory,
    process::{BirthState, ProcessIdentity},
};
use std::{
    fs,
    io::Write,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    process::Command,
};
use support::*;

#[test]
#[ignore = "owned subprocess stopped or execed at a selected physical publication cut"]
fn writer_child() {
    let home = env_path("AGENTCHAT_HOME");
    let parent = env_path("AGENTCHAT_ORIGIN_PARENT");
    let stage = std::env::var("AGENTCHAT_ORIGIN_STAGE").unwrap();
    let phase = std::env::var("AGENTCHAT_ORIGIN_PHASE").unwrap();
    let purpose =
        serde_json::from_str(&std::env::var("AGENTCHAT_ORIGIN_PURPOSE").unwrap()).unwrap();
    let data = if purpose == StagePurpose::Signal {
        b"0\n".as_slice()
    } else {
        DATA
    };
    let guard = if phase == "guard" {
        let file = Directory::shared(&parent)
            .unwrap()
            .create_record("lifecycle.lock".as_ref())
            .unwrap();
        file.sync_all().unwrap();
        Some(file)
    } else {
        None
    };
    let mut origin = OriginRegistry::new(home)
        .begin_file(FilePlan {
            parent: &parent,
            stage: stage.as_ref(),
            target: "canonical".as_ref(),
            purpose,
            alternate_parent: Some(&parent.with_file_name("quarantine")),
            guard: guard.as_ref().map(|_| "lifecycle.lock".as_ref()),
        })
        .unwrap();
    let file = if phase != "intent" {
        let mut file = Directory::shared(&parent)
            .unwrap()
            .create_record(stage.as_ref())
            .unwrap();
        file.write_all(data).unwrap();
        file.sync_all().unwrap();
        if phase != "allocated" {
            origin.seal(&file, data).unwrap();
        }
        Some(file)
    } else {
        None
    };
    if phase == "context-published" {
        // Deliberately stop before syncing the renamed immutable entry. The
        // collector must establish that durability before dropping its proof.
        Directory::shared(&parent)
            .unwrap()
            .publish_new_io(stage.as_ref(), "canonical".as_ref())
            .unwrap();
    }
    let token = ProcessIdentity::capture(std::process::id()).unwrap();
    let marker = env_path("AGENTCHAT_ORIGIN_MARKER");
    let inherited = if phase == "inherit" {
        // Only this fixture deliberately exports the original locked FD. An
        // unexpectedly inherited writer must block orphan unlink even after
        // positive physical birth loss of its original publisher.
        rustix::io::fcntl_setfd(file.as_ref().unwrap(), rustix::io::FdFlags::empty()).unwrap();
        let child = Command::new("/bin/cat")
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let expected = std::path::Path::new("/bin/cat").canonicalize().unwrap();
        let mut captured = None;
        wait(|| {
            captured = ProcessIdentity::capture(child.id())
                .ok()
                .filter(|t| t.executable() == expected && t.is_current());
            captured.is_some()
        });
        fs::write(
            marker.with_extension("inherited"),
            serde_json::to_vec(&captured.unwrap()).unwrap(),
        )
        .unwrap();
        Some(child)
    } else {
        None
    };
    let temporary = marker.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec(&token).unwrap()).unwrap();
    fs::rename(temporary, marker).unwrap();
    if phase == "exec" {
        panic!("exec failed: {}", Command::new("/bin/cat").exec());
    }
    std::hint::black_box((&origin, &file, &inherited, &guard));
    loop {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn actual_dead_complete_writer_is_recovered_without_mutating_canonical_state() {
    let mut fixture = Fixture::start("sealed", StagePurpose::Header);
    fs::write(
        fixture.parent.join("canonical"),
        b"committed generation/floor",
    )
    .unwrap();
    fixture.kill();
    let report = fixture.collect();
    assert_eq!(
        (report.visited, report.removed, report.preserved),
        (1, 2, 0)
    );
    assert!(!fixture.stage().exists());
    assert_eq!(
        fs::read(fixture.parent.join("canonical")).unwrap(),
        b"committed generation/floor"
    );
    assert!(
        fs::read_dir(fixture.home.join("staging"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn live_writer_and_same_birth_exec_are_preserved_until_positive_death() {
    for phase in ["sealed", "exec"] {
        let mut fixture = Fixture::start(phase, StagePurpose::Header);
        if phase == "exec" {
            wait(|| !fixture.creator.is_current());
            assert_eq!(fixture.creator.birth_state(), BirthState::Present);
        }
        let report = fixture.collect();
        assert_eq!((report.removed, report.preserved), (0, 1));
        assert_eq!(fs::read(fixture.stage()).unwrap(), DATA);
        fixture.kill();
        assert_eq!(fixture.collect().removed, 2);
    }
}

#[test]
fn intent_without_data_retires_but_unsealed_allocations_are_diagnostic_only() {
    for phase in ["intent", "allocated"] {
        let mut fixture = Fixture::start(phase, StagePurpose::Header);
        fixture.kill();
        let before = fs::read_dir(fixture.home.join("staging")).unwrap().count();
        assert_eq!(before, 1);
        let mut reader = OriginReaper::new(&fixture.home);
        let page = reader.inspect().unwrap();
        assert_eq!(page.removed, 0);
        assert_eq!(
            fs::read_dir(fixture.home.join("staging")).unwrap().count(),
            1
        );
        let page = fixture.collect();
        if phase == "intent" {
            assert_eq!(page.removed, 1);
        } else {
            assert_eq!((page.removed, page.ambiguous), (0, 1));
            assert_eq!(fs::read(fixture.stage()).unwrap(), DATA);
        }
    }
}

#[test]
fn foreign_replacements_edits_aliases_symlinks_and_parents_survive() {
    for change in ["replace", "edit", "alias", "symlink", "parent"] {
        let mut fixture = Fixture::start("sealed", StagePurpose::Header);
        fixture.kill();
        let path = fixture.stage();
        let displaced = fixture.root.path().join("displaced");
        match change {
            "replace" => {
                fs::rename(&path, &displaced).unwrap();
                fs::write(&path, DATA).unwrap();
            }
            "edit" => {
                fs::write(&path, DATA).unwrap();
            }
            "alias" => fs::hard_link(&path, &displaced).unwrap(),
            "symlink" => {
                fs::rename(&path, &displaced).unwrap();
                std::os::unix::fs::symlink(&displaced, &path).unwrap();
            }
            "parent" => {
                fs::rename(&fixture.parent, &displaced).unwrap();
                fs::create_dir(&fixture.parent).unwrap();
                fs::write(&path, DATA).unwrap();
            }
            _ => unreachable!(),
        }
        if change == "replace" {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let report = fixture.collect();
        assert_eq!((report.removed, report.ambiguous), (0, 1), "{change}");
        assert_eq!(fs::read(&path).unwrap(), DATA);
        assert_eq!(
            fs::read_dir(fixture.home.join("staging")).unwrap().count(),
            1
        );
    }
}

#[test]
fn complete_context_signal_and_socket_witnesses_need_their_own_recovery() {
    for purpose in [
        StagePurpose::Context,
        StagePurpose::Signal,
        StagePurpose::SignalLock,
        StagePurpose::Relay,
        StagePurpose::Gateway,
    ] {
        // Signal's exact two-byte protocol is tested separately by signal genesis.
        if purpose == StagePurpose::Signal {
            continue;
        }
        let mut fixture = Fixture::start("sealed", purpose);
        fixture.kill();
        let report = fixture.collect();
        assert_eq!((report.removed, report.pending_policy), (0, 1));
        assert_eq!(fs::read(fixture.stage()).unwrap(), DATA);
    }
}

#[test]
fn original_parent_quarantine_and_partial_unlink_are_recoverable() {
    let mut fixture = Fixture::start("sealed", StagePurpose::Header);
    fixture.kill();
    let quarantine = fixture.parent.with_file_name("quarantine");
    fs::rename(&fixture.parent, &quarantine).unwrap();
    assert_eq!(fixture.collect().removed, 2);
    assert!(fs::read_dir(quarantine).unwrap().next().is_none());
    let mut fixture = Fixture::start("sealed", StagePurpose::Header);
    fixture.kill();
    fs::remove_file(fixture.stage()).unwrap();
    assert_eq!(fixture.collect().removed, 1);
}

#[test]
fn changed_index_copy_and_held_original_stage_lock_preserve_data() {
    let mut fixture = Fixture::start("sealed", StagePurpose::Header);
    fixture.kill();
    let index = fixture.home.join("staging");
    let original = fs::read_dir(&index)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let copied = index.join(format!("{}.json", uuid::Uuid::new_v4()));
    fs::copy(&original, &copied).unwrap();
    fs::set_permissions(&copied, fs::Permissions::from_mode(0o600)).unwrap();
    let directory = Directory::shared(&fixture.parent).unwrap();
    let snapshot = directory
        .read(fixture.stage_name.as_ref())
        .unwrap()
        .unwrap();
    let held = directory
        .lock_record(fixture.stage_name.as_ref(), &snapshot)
        .unwrap();
    let report = fixture.collect();
    assert_eq!(report.removed, 0);
    assert_eq!(fs::read(fixture.stage()).unwrap(), DATA);
    drop(held);
    let copied_bytes = fs::read(&copied).unwrap();
    let mut collector = OriginReaper::new(&fixture.home);
    let mut removed = 0;
    // Nonblocking maintenance may preserve busy or uncertain observations.
    // Releasing this FD does not promise every observation is immediately
    // available; require bounded eventual progress, with safety on every pass.
    wait(|| {
        let report = collector.step().unwrap();
        assert!(report.visited <= 8 && report.removed <= 16);
        removed += report.removed;
        assert!(removed <= 2);
        assert_eq!(fs::read(&copied).unwrap(), copied_bytes);
        removed == 2
    });
    assert!(!fixture.stage().exists());
    assert!(copied.exists());
    assert_eq!(fs::read_dir(index).unwrap().count(), 1);
}

#[test]
fn retained_cursor_advances_beyond_live_front_entries_with_one_overall_budget() {
    let mut fixture = Fixture::start("sealed", StagePurpose::Header);
    let mut live = Vec::new();
    for n in 0..18 {
        let stage = format!(".live-{n}");
        live.push(
            OriginRegistry::new(&fixture.home)
                .begin_file(FilePlan {
                    parent: &fixture.parent,
                    stage: stage.as_ref(),
                    target: "canonical".as_ref(),
                    purpose: StagePurpose::Header,
                    alternate_parent: None,
                    guard: None,
                })
                .unwrap(),
        );
    }
    fixture.kill();
    let mut collector = OriginReaper::new(&fixture.home);
    let mut visited = 0;
    let mut removed = 0;
    for _ in 0..3 {
        let page = collector.step().unwrap();
        assert!(page.visited <= 8 && page.removed <= 16);
        visited += page.visited;
        removed += page.removed;
    }
    assert_eq!((visited, removed), (19, 2));
    assert!(!fixture.stage().exists());
    assert_eq!(
        fs::read_dir(fixture.home.join("staging")).unwrap().count(),
        18
    );
    drop(live);
    assert_eq!(
        fs::read_dir(fixture.home.join("staging")).unwrap().count(),
        0
    );
}

#[test]
fn explicit_writer_finish_retires_only_its_intent_and_never_the_canonical_file() {
    use std::os::unix::fs::DirBuilderExt;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap().join("state");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let mut origin = OriginRegistry::new(&home)
        .begin_file(FilePlan {
            parent: &home,
            stage: ".planned".as_ref(),
            target: "canonical".as_ref(),
            purpose: StagePurpose::Header,
            alternate_parent: None,
            guard: None,
        })
        .unwrap();
    let parent = Directory::owned(&home).unwrap();
    let mut file = parent.create_record(".planned".as_ref()).unwrap();
    file.write_all(DATA).unwrap();
    origin.seal(&file, DATA).unwrap();
    assert!(origin.finish().is_err());
    assert_eq!(fs::read(home.join(".planned")).unwrap(), DATA);
    parent
        .publish_new(".planned".as_ref(), "canonical".as_ref())
        .unwrap();
    parent.sync().unwrap();
    origin.finish().unwrap();
    drop(origin);
    assert_eq!(fs::read(home.join("canonical")).unwrap(), DATA);
    assert_eq!(fs::read_dir(home.join("staging")).unwrap().count(), 0);
}

#[test]
fn unknown_original_birth_preserves_complete_data_and_intent() {
    let mut fixture = Fixture::start("sealed", StagePurpose::Header);
    fixture.kill();
    let index = Directory::owned(&fixture.home.join("staging")).unwrap();
    let entry = fs::read_dir(&index.path)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    let snapshot = index.read(&entry).unwrap().unwrap();
    let mut value = super::record::Record::decode(&index, &entry, &snapshot).unwrap();
    let mut saved = serde_json::to_value(&value.creator).unwrap();
    saved["pid"] = 0.into();
    value.creator = serde_json::from_value(saved).unwrap();
    assert_eq!(value.creator.birth_state(), BirthState::Unknown);
    fs::write(index.path.join(&entry), value.seal().unwrap()).unwrap();
    let page = fixture.collect();
    assert_eq!((page.removed, page.preserved, page.ambiguous), (0, 1, 0));
    assert_eq!(fs::read(fixture.stage()).unwrap(), DATA);
    assert!(index.path.join(entry).exists());
}

#[test]
fn actual_inherited_original_stage_lock_blocks_cleanup_until_its_holder_exits() {
    let mut fixture = Fixture::start("inherit", StagePurpose::Header);
    let input = fixture.detach_input();
    let inherited: ProcessIdentity =
        serde_json::from_slice(&fs::read(fixture.root.path().join("ready.inherited")).unwrap())
            .unwrap();
    assert_eq!(inherited.birth_state(), BirthState::Present);
    fixture.kill();
    assert_eq!(fixture.collect().removed, 0);
    assert_eq!(fs::read(fixture.stage()).unwrap(), DATA);
    assert_eq!(inherited.birth_state(), BirthState::Present);
    drop(input);
    wait(|| inherited.birth_state() == BirthState::Gone);
    assert_eq!(fixture.collect().removed, 2);
}

#[test]
fn original_lifecycle_guard_serializes_cleanup_and_a_successor_lock_is_preserved() {
    let mut fixture = Fixture::start("guard", StagePurpose::Header);
    fixture.kill();
    let parent = Directory::shared(&fixture.parent).unwrap();
    let name = std::ffi::OsStr::new("lifecycle.lock");
    let snapshot = parent.read(name).unwrap().unwrap();
    let held = parent.lock_record(name, &snapshot).unwrap();
    assert_eq!(fixture.collect().removed, 0);
    assert_eq!(fs::read(fixture.stage()).unwrap(), DATA);
    drop(held);
    let mut reaper = OriginReaper::new(&fixture.home);
    let mut removed = 0;
    // Releasing our lifecycle lock permits future collection; a nonblocking
    // pass may still preserve uncertain/busy observations. Assert the finite
    // retry contract and original guard bytes on every pass.
    wait(|| {
        let page = reaper.step().unwrap();
        assert!(page.visited <= 8 && page.removed <= 16);
        removed += page.removed;
        assert!(removed <= 2);
        assert!(parent.matches(name, &snapshot).unwrap());
        removed == 2
    });
    assert!(parent.matches(name, &snapshot).unwrap());
    let mut fixture = Fixture::start("guard", StagePurpose::Header);
    fixture.kill();
    let name = fixture.parent.join("lifecycle.lock");
    fs::rename(&name, fixture.root.path().join("original-guard")).unwrap();
    let successor = Directory::shared(&fixture.parent)
        .unwrap()
        .create_record("lifecycle.lock".as_ref())
        .unwrap();
    successor.sync_all().unwrap();
    let report = fixture.collect();
    assert_eq!((report.removed, report.ambiguous), (0, 1));
    assert_eq!(fs::read(fixture.stage()).unwrap(), DATA);
}

#[test]
fn complete_absent_signal_and_socket_stages_keep_their_role_specific_witnesses() {
    for purpose in [
        StagePurpose::Signal,
        StagePurpose::SignalLock,
        StagePurpose::Relay,
        StagePurpose::Gateway,
    ] {
        let mut fixture = Fixture::start("sealed", purpose);
        fixture.kill();
        fs::remove_file(fixture.stage()).unwrap();
        let page = fixture.collect();
        assert_eq!(page.removed, 0);
        assert_eq!(page.pending_policy, 1);
        assert_eq!(
            fs::read_dir(fixture.home.join("staging")).unwrap().count(),
            1
        );
    }
}
