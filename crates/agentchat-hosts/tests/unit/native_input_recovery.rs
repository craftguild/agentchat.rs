//! Original-file receipt recovery and bounded retention; no native host admission.
use super::*;
use crate::claude::{
    NativeRegistration,
    body::{InputReceipt, NativeInputResult, retire_scope},
    metadata,
};
use serde_json::json;
use std::{fs, os::unix::fs::DirBuilderExt};
use uuid::Uuid;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, metadata::Publisher) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap().join("state");
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    let publisher = metadata::Publisher::new(path.clone());
    (root, path, publisher)
}
fn receipt(file: &InputFile) -> InputReceipt {
    InputReceipt::from_native(
        file,
        NativeInputResult::Append(json!({
            "uuid":Uuid::new_v4(),"message":{"type":"user","role":"user","isMeta":true,
            "content":[{"type":"text","text":file.text}]}
        })),
    )
    .unwrap()
}
fn scope(file: &InputFile) -> NativeRegistration {
    NativeRegistration {
        owner_root: Uuid::new_v4(),
        observer: "physical-fixture-only".into(),
        generation: file.native_generation,
        root: file.root,
        context: Context {
            context_id: file.context_id,
            incarnation: file.incarnation,
            project_id: file.input().unwrap().project_id(),
            host: agentchat_core::model::HostKind::Claude,
            host_session_id: file.root.to_string(),
            host_version: "2.1.302".into(),
            os_profile: "test-only".into(),
            working_directory: "/test-owned/workspace".into(),
        },
    }
}
fn publish(file: &InputFile, publisher: &metadata::Publisher, directory: &std::path::Path) {
    file.publish(publisher, directory).unwrap();
    InputHead::new(file)
        .unwrap()
        .publish(publisher, directory)
        .unwrap();
    PendingInput::new(file, 1)
        .unwrap()
        .publish(publisher, directory)
        .unwrap();
}
#[test]
fn receipt_survives_scope_retirement_and_is_read_only_across_connections() {
    let (_root, directory, publisher) = fixture();
    let file = crate::claude::body::tests::input();
    publish(&file, &publisher, &directory);
    receipt(&file).publish(&publisher, &directory).unwrap();
    assert_eq!(retire_scope(&directory, &scope(&file)).unwrap(), 1);
    assert_eq!(retire_scope(&directory, &scope(&file)).unwrap(), 0);
    assert!(!PendingInput::path(&directory, file.context_id).exists());
    let bytes = fs::read(file.path(&directory)).unwrap();
    let mut current = file.clone();
    current.context_id = Uuid::new_v4();
    current.incarnation = Uuid::new_v4();
    current.native_generation += 1;
    assert!(matches!(
        recover(&directory, &current, true).unwrap(),
        Some(MessageProgress::Accepted(_))
    ));
    assert!(recover(&directory, &current, false).unwrap().is_none());
    assert_eq!(fs::read(file.path(&directory)).unwrap(), bytes);
    assert!(!current.path(&directory).exists());
    assert!(!PendingInput::path(&directory, current.context_id).exists());
    for change in ["root", "token", "generation", "body", "delivery", "project"] {
        let mut wrong = current.clone();
        let mut message = wrong.message().unwrap();
        match change {
            "root" => wrong.root = Uuid::new_v4(),
            "token" => wrong.token = Uuid::new_v4(),
            "generation" => message.generation = Uuid::new_v4(),
            "body" => message.body = "changed whole body".into(),
            "delivery" => message.delivery_id = Uuid::new_v4(),
            "project" => message.project_id = Uuid::new_v4(),
            _ => unreachable!(),
        }
        wrong.text = message.text().unwrap();
        assert!(
            recover(&directory, &wrong, true).unwrap().is_none(),
            "{change}"
        );
    }
}
#[test]
fn old_scope_without_native_success_and_legacy_failure_stay_unknown() {
    let (_root, directory, publisher) = fixture();
    let file = crate::claude::body::tests::input();
    publish(&file, &publisher, &directory);
    assert!(matches!(
        recover(&directory, &file, true).unwrap(),
        Some(MessageProgress::AwaitingAcceptance)
    ));
    retire_scope(&directory, &scope(&file)).unwrap();
    let mut current = file.clone();
    current.context_id = Uuid::new_v4();
    current.incarnation = Uuid::new_v4();
    current.native_generation += 1;
    assert!(recover(&directory, &current, true).unwrap().is_none());
    InputReceipt::from_native(&file, NativeInputResult::Failed)
        .unwrap()
        .publish(&publisher, &directory)
        .unwrap();
    assert!(matches!(
        recover(&directory, &current, true).unwrap(),
        Some(MessageProgress::AwaitingAcceptance)
    ));
    let legacy =
        InputReceipt::read(&InputReceipt::path(&directory, file.context_id, file.token)).unwrap();
    assert!(!legacy.rejected_before_entry());
    legacy.remove(&directory).unwrap();
    InputReceipt::from_native(&file, NativeInputResult::RejectedBeforeEntry)
        .unwrap()
        .publish(&publisher, &directory)
        .unwrap();
    assert!(matches!(
        recover(&directory, &current, true).unwrap(),
        Some(MessageProgress::NotAccepted(_))
    ));
}
#[test]
fn one_owner_retains_one_original_body_and_result_across_many_native_scopes() {
    let (_root, directory, publisher) = fixture();
    let mut file = crate::claude::body::tests::input();
    for _ in 0..20 {
        if let Some(prior) = InputHead::read(&directory).unwrap() {
            assert_eq!(prior.remove_transport(&directory).unwrap(), 3);
        }
        publish(&file, &publisher, &directory);
        receipt(&file).publish(&publisher, &directory).unwrap();
        retire_scope(&directory, &scope(&file)).unwrap();
        let names: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("input-") && n.ends_with(".json"))
                .count(),
            2
        );
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("accepted-input-") && n.ends_with(".json"))
                .count(),
            1
        );
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("pending-input-"))
                .count(),
            0
        );
        file.context_id = Uuid::new_v4();
        file.incarnation = Uuid::new_v4();
        file.token = Uuid::new_v4();
        file.native_generation += 1;
    }
}
#[test]
fn original_head_cleanup_recovers_partial_prefixes_and_preserves_foreign_payloads() {
    for missing in ["none", "body", "receipt", "both"] {
        let (_root, directory, publisher) = fixture();
        let file = crate::claude::body::tests::input();
        publish(&file, &publisher, &directory);
        let receipt = receipt(&file);
        receipt.publish(&publisher, &directory).unwrap();
        let head = InputHead::read(&directory).unwrap().unwrap();
        if missing == "body" || missing == "both" {
            file.remove(&directory).unwrap();
        }
        if missing == "receipt" || missing == "both" {
            receipt.remove(&directory).unwrap();
        }
        head.remove_transport(&directory).unwrap();
        assert!(InputHead::read(&directory).unwrap().is_none());
        assert!(!file.path(&directory).exists());
        assert!(!InputReceipt::path(&directory, file.context_id, file.token).exists());
    }
    let (_root, directory, publisher) = fixture();
    let file = crate::claude::body::tests::input();
    publish(&file, &publisher, &directory);
    let head = InputHead::read(&directory).unwrap().unwrap();
    let foreign = b"foreign replacement";
    fs::remove_file(file.path(&directory)).unwrap();
    fs::write(file.path(&directory), foreign).unwrap();
    assert!(head.remove_transport(&directory).is_err());
    assert_eq!(fs::read(file.path(&directory)).unwrap(), foreign);
    assert_eq!(InputHead::read(&directory).unwrap().unwrap(), head);
}

#[test]
fn original_result_requires_the_exact_current_frame_and_claim_correlation() {
    let (_root, directory, publisher) = fixture();
    let file = crate::claude::body::tests::input();
    publish(&file, &publisher, &directory);
    receipt(&file).publish(&publisher, &directory).unwrap();
    let dispatch = NativeDispatch::new(
        file.input().unwrap(),
        file.token,
        1,
        agentchat_core::delivery::Presentation::Idle,
        true,
    );
    let root = file.root.to_string();
    let (retained, result) = original_result(&directory, &root, &dispatch).unwrap();
    assert_eq!(retained, file);
    assert!(result.unwrap().accepted);
    let before = fs::read(file.path(&directory)).unwrap();
    for change in [
        "token",
        "project",
        "delivery",
        "generation",
        "body",
        "fresh",
    ] {
        let mut wrong = dispatch.clone();
        let NativeDispatch::Message(value) = &mut wrong else {
            unreachable!()
        };
        match change {
            "token" => value.token = Uuid::new_v4(),
            "project" => value.input.project_id = Uuid::new_v4(),
            "delivery" => value.input.delivery_id = Uuid::new_v4(),
            "generation" => value.input.generation = Uuid::new_v4(),
            "body" => value.input.body = "changed".into(),
            "fresh" => value.awaiting_acceptance = false,
            _ => unreachable!(),
        }
        assert!(
            original_result(&directory, &root, &wrong).is_err(),
            "{change}"
        );
    }
    assert!(original_result(&directory, &Uuid::new_v4().to_string(), &dispatch).is_err());
    assert_eq!(fs::read(file.path(&directory)).unwrap(), before);
}
