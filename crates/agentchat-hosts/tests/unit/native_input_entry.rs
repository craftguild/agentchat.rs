//! Durable entry intent is a one-shot fence and cannot manufacture a receipt.
use super::*;
use std::{fs, os::unix::fs::DirBuilderExt};
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, metadata::Publisher) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap().join("state");
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    let publisher = metadata::Publisher::new(path.clone());
    (root, path, publisher)
}
#[test]
fn original_intent_survives_reopen_without_renewing_an_entry_or_receipt() {
    let (_root, directory, publisher) = fixture();
    let file = crate::claude::body::tests::input();
    assert!(file.supports_entry_claim());
    file.publish(&publisher, &directory).unwrap();
    let head = InputHead::new(&file).unwrap();
    head.publish(&publisher, &directory).unwrap();
    assert!(InputEntry::claim(&file, &publisher, &directory).unwrap());
    let path = InputEntry::path_for(&directory, &file).unwrap();
    let original = fs::read(&path).unwrap();
    let reopened = InputEntry::read(&path).unwrap();
    assert!(reopened.matches(&file).unwrap());
    assert!(
        !InputEntry::claim(
            &file,
            &metadata::Publisher::new(directory.clone()),
            &directory
        )
        .unwrap()
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(head.receipt(&directory, &file).unwrap().is_none());
    assert_eq!(head.remove_transport(&directory).unwrap(), 3);
    assert!(!path.exists());
}
#[test]
fn intent_is_bound_to_the_whole_body_and_foreign_metadata_is_preserved() {
    let (_root, directory, publisher) = fixture();
    let file = crate::claude::body::tests::input();
    InputEntry::claim(&file, &publisher, &directory).unwrap();
    let path = InputEntry::path_for(&directory, &file).unwrap();
    let entry = InputEntry::read(&path).unwrap();
    let mut changed = file.clone();
    let mut message = changed.message().unwrap();
    message.body = "changed".into();
    changed.text = message.text().unwrap();
    assert!(!entry.matches(&changed).unwrap());
    assert!(InputEntry::claim(&changed, &publisher, &directory).is_err());
    let bytes = fs::read(&path).unwrap();
    let copy = directory.join("copied.json");
    fs::copy(&path, &copy).unwrap();
    assert!(InputEntry::read(&copy).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let mut legacy = serde_json::to_value(&file).unwrap();
    legacy["version"] = 1.into();
    let legacy: InputFile = serde_json::from_value(legacy).unwrap();
    assert!(legacy.message().is_err());
    assert!(legacy.input().is_err());
    assert!(legacy.notice().is_err());
    assert!(!legacy.supports_entry_claim());
    assert!(InputEntry::claim(&legacy, &publisher, &directory).is_err());
}
