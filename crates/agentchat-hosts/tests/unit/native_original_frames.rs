//! New native frames reject old policy metadata and capsule revisions.
use super::*;
use crate::claude::body::{InputEntry, NativeInputResult, PendingInput};

#[test]
fn retained_dispatch_v4_separators_keep_capsule_bytes_and_original_receipt_digest() {
    use std::os::unix::fs::DirBuilderExt;
    let compact = super::tests::input();
    let mut retained = compact.clone();
    let middle = compact
        .text
        .strip_prefix("[AgentChat:Message]\n")
        .unwrap()
        .strip_suffix("\n[/AgentChat:Message]")
        .unwrap();
    retained.text = format!("[AgentChat:Message]\n--\n{middle}\n--\n[/AgentChat:Message]");
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("state");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let path = home.join("input");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    let publisher = Publisher::new(home);
    retained.publish(&publisher, &path).unwrap();
    let original_bytes = std::fs::read(retained.path(&path)).unwrap();
    let loaded = InputFile::read(&retained.path(&path)).unwrap();
    assert_eq!(loaded, retained);
    assert_eq!(loaded.input().unwrap(), compact.input().unwrap());
    let receipt = InputReceipt::from_native(
        &loaded,
        NativeInputResult::Append(serde_json::json!({
            "uuid": Uuid::new_v4(),
            "message": {"type":"user", "role":"user", "isMeta":true,
                "content":[{"type":"text", "text":loaded.text}]}
        })),
    )
    .unwrap();
    assert!(receipt.accepted);
    assert!(receipt.matches(&loaded).unwrap());
    assert!(!receipt.matches(&compact).unwrap());
    assert_ne!(
        loaded.notice_proof().unwrap(),
        compact.notice_proof().unwrap()
    );
    assert_eq!(std::fs::read(retained.path(&path)).unwrap(), original_bytes);
}

#[test]
fn old_capsule_versions_have_no_entry_publication_or_result_permission() {
    let file = super::tests::input();
    assert_eq!(file.version, 5);
    for version in [1, 2, 3, 4, 6] {
        let mut old = file.clone();
        old.version = version;
        assert_eq!(
            old.input().unwrap_err().code,
            ErrorCode::UnsupportedProtocol,
            "version {version}"
        );
        assert!(old.original_input(NativeFrameRevision::DispatchV4).is_err());
        assert!(!old.supports_entry_claim());
        assert!(old.notice().is_err());
        assert!(PendingInput::new(&old, 1).is_err());
        assert!(InputReceipt::from_native(&old, NativeInputResult::Failed).is_err());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap();
        let publisher = Publisher::new(path.clone());
        assert!(old.publish(&publisher, &path).is_err());
        assert!(InputEntry::claim(&old, &publisher, &path).is_err());
    }
}

#[test]
fn old_ack_metadata_and_missing_read_policy_are_rejected() {
    let file = super::tests::input();
    let message = file.message().unwrap();
    assert_eq!(message.body.as_bytes(), &[0; 65_536]);
    assert!(message.auto_read);
    for text in [
        file.text.replacen('{', "{\"auto_ack\":true,", 1),
        file.text.replace("\"auto_read\":true,", ""),
    ] {
        assert!(MessageInput::from_text(&text).is_err());
        let mut old = file.clone();
        old.text = text;
        assert!(old.input().is_err());
        assert!(old.original_input(NativeFrameRevision::DispatchV4).is_err());
    }
    for code in [0, 1, 2, 3, 5] {
        assert!(NativeFrameRevision::from_code(code).is_err());
    }
    assert_eq!(NativeFrameRevision::DispatchV4.code(), 4);
}
