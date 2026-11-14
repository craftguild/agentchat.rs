//! Typed full-input capsules and real physical provenance, not host admission.
use super::*;
use agentchat_core::{
    delivery::{NativeInput, NoticeInput, Presentation},
    model::{Dispatch, DispatchKind, DispatchNotice, NoticeKind},
    read::IdentityLabel,
    sequence::Sequence,
    time::Timestamp,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink},
};

pub(crate) fn input() -> InputFile {
    let id = Uuid::from_u128;
    let root = id(1);
    let context = Context {
        context_id: id(2),
        project_id: id(3),
        host: HostKind::Claude,
        host_session_id: root.to_string(),
        host_version: "2.1.302".into(),
        os_profile: "test-only".into(),
        working_directory: "/test-owned/workspace".into(),
        incarnation: id(4),
    };
    let dispatch = MessageDispatch {
        input: MessageInput {
            project_id: context.project_id,
            generation: id(5),
            delivery_id: id(6),

            auto_read: true,
            frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
            dispatch_id: id(8),
            seq: Sequence::new(9_007_199_254_740_993).unwrap(),
            message_id: id(8),
            thread_id: id(8),
            author: IdentityLabel {
                actor_id: id(9),
                kind: agentchat_core::model::ActorKind::Agent,
                handle: "author".into(),
            },
            channel_id: id(10),
            channel: "dev".into(),
            body: "\0".repeat(65_536),
            addressed: false,
        },
        token: id(11),
        attempt: 1,
        presentation: Presentation::Idle,
        awaiting_acceptance: false,
        frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
    };
    InputFile::new(&context, root, 1, &dispatch).unwrap()
}

fn fixture() -> (tempfile::TempDir, PathBuf, Publisher) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap().join("state");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let directory = home.join("input");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let publisher = Publisher::new(home);
    (root, directory, publisher)
}

#[test]
fn maximum_escaped_body_has_a_separate_budget_without_widening_metadata() {
    let (_root, directory, publisher) = fixture();
    let input = input();
    input.publish(&publisher, &directory).unwrap();
    let path = input.path(&directory);
    let bytes = fs::read(&path).unwrap();
    assert!(bytes.len() > 393_216 && bytes.len() < 524_288);
    assert_eq!(InputFile::read(&path).unwrap(), input);
    let decoded = input.message().unwrap();
    assert_eq!(decoded.body.as_bytes(), &[0u8; 65_536]);
    assert_eq!(decoded.seq.get(), 9_007_199_254_740_993);
    assert!(metadata::read::<serde_json::Value>(&path).is_err());
    assert!(metadata::write(&path, &serde_json::json!({"replacement": true})).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
    let ordinary = directory.join("ordinary.json");
    assert!(publisher.create(&ordinary, &"x".repeat(65_536)).is_err());
    assert!(!ordinary.exists());
}

#[test]
fn typed_notice_round_trips_through_the_original_capsule_without_becoming_a_message() {
    let (_root, directory, publisher) = fixture();
    let mut file = input();
    let original = file.message().unwrap();
    let notice = NoticeInput::new(
        original.project_id,
        original.generation,
        original.delivery_id,
        &Dispatch {
            id: original.dispatch_id,
            kind: DispatchKind::Notice,
            channel_id: Some(original.channel_id),
            creation_seq: original.seq,
            created_at: Timestamp::parse("2026-10-19T00:00:00.000Z").unwrap(),
        },
        &DispatchNotice {
            dispatch_id: original.dispatch_id,
            kind: NoticeKind::ChannelJoined,
            data: serde_json::json!({"agent_id":original.author.actor_id,"channel_id":original.channel_id}),
        },
        true,
        agentchat_core::project::ProjectReceiptSettings::default().input_policy(),
    )
    .unwrap();
    file.text = notice.text().unwrap();
    assert_eq!(file.input().unwrap(), NativeInput::Notice(notice));
    assert!(file.message().is_err());
    file.publish(&publisher, &directory).unwrap();
    assert_eq!(InputFile::read(&file.path(&directory)).unwrap(), file);
}

#[test]
fn exact_retry_keeps_original_inode_and_requires_parent_durability() {
    let (_root, directory, publisher) = fixture();
    let input = input();
    input.publish(&publisher, &directory).unwrap();
    let path = input.path(&directory);
    let bytes = fs::read(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    let refused = crate::staging::tests::durability::Refusal::at(&directory);
    assert!(input.publish(&publisher, &directory).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    drop(refused);
    input.publish(&publisher, &directory).unwrap();
    assert_eq!(fs::metadata(path).unwrap().ino(), inode);
}

#[test]
fn changed_copied_linked_legacy_and_reparented_capsules_are_never_adopted() {
    for mode in ["edited", "copy", "hardlink", "symlink", "legacy", "parent"] {
        let (root, directory, publisher) = fixture();
        let input = input();
        input.publish(&publisher, &directory).unwrap();
        let path = input.path(&directory);
        let original = fs::read(&path).unwrap();
        let moved = root.path().join("original");
        match mode {
            "edited" => {
                let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
                value["text"] = "changed".into();
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "copy" => {
                fs::rename(&path, &moved).unwrap();
                fs::copy(&moved, &path).unwrap();
            }
            "hardlink" => fs::hard_link(&path, &moved).unwrap(),
            "symlink" => {
                fs::rename(&path, &moved).unwrap();
                symlink(&moved, &path).unwrap();
            }
            "legacy" => fs::write(&path, serde_json::to_vec(&input).unwrap()).unwrap(),
            "parent" => {
                fs::rename(&directory, &moved).unwrap();
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .unwrap();
                fs::copy(input.path(&moved), &path).unwrap();
            }
            _ => unreachable!(),
        }
        if mode != "symlink" {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let changed = fs::read(&path).unwrap();
        assert!(InputFile::read(&path).is_err(), "{mode}");
        assert!(input.publish(&publisher, &directory).is_err(), "{mode}");
        assert!(input.remove(&directory).is_err(), "{mode}");
        assert_eq!(fs::read(path).unwrap(), changed, "{mode}");
    }
}

#[test]
fn separate_tokens_keep_whole_bodies_and_exact_removal_preserves_successors() {
    let (_root, directory, publisher) = fixture();
    let first = input();
    let mut second = first.clone();
    second.token = Uuid::new_v4();
    let mut message = first.message().unwrap();
    message.body = " \r\nSecond complete body\n--\n[/AgentChat:Message]\n末尾 \t\n".into();
    second.text = message.text().unwrap();
    first.publish(&publisher, &directory).unwrap();
    second.publish(&publisher, &directory).unwrap();
    assert_eq!(InputFile::read(&first.path(&directory)).unwrap(), first);
    assert_eq!(InputFile::read(&second.path(&directory)).unwrap(), second);
    let mut wrong = second.clone();
    wrong.token = first.token;
    assert!(!wrong.remove(&directory).unwrap());
    assert_eq!(InputFile::read(&first.path(&directory)).unwrap(), first);
    assert!(first.remove(&directory).unwrap());
    assert!(!first.remove(&directory).unwrap());
    assert_eq!(InputFile::read(&second.path(&directory)).unwrap(), second);
}

#[test]
fn invalid_tokens_roots_native_generations_and_noncanonical_body_are_refused() {
    let (_root, directory, publisher) = fixture();
    for mode in ["token", "root", "zero", "overflow", "text", "body"] {
        let mut file = input();
        match mode {
            "token" => file.token = Uuid::nil(),
            "root" => file.root = Uuid::nil(),
            "zero" => file.native_generation = 0,
            "overflow" => file.native_generation = 9_007_199_254_740_992,
            "text" => file.text.insert(0, ' '),
            "body" => {
                let mut message = file.message().unwrap();
                message.body.push('x');
                let json = serde_json::to_string(&message).unwrap();
                file.text = format!("{}\n{json}", file.text.split_once('\n').unwrap().0);
            }
            _ => unreachable!(),
        }
        assert!(file.publish(&publisher, &directory).is_err(), "{mode}");
        assert!(!file.path(&directory).exists(), "{mode}");
    }
}
