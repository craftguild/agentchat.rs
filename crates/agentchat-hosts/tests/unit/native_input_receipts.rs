//! Native whole-body results and durable receipt/pointer correlation.
use super::*;
use serde_json::{Value, json};
fn file(addressed: bool) -> InputFile {
    let mut file = super::tests::input();
    let mut message = file.message().unwrap();
    message.addressed = addressed;
    file.text = message.text().unwrap();
    file
}
fn append(file: &InputFile) -> Value {
    json!({"uuid":Uuid::new_v4(),"message":{"type":"user","role":"user","isMeta":true,"content":[{"type":"text","text":file.text}]}})
}
fn queued(file: &InputFile) -> Value {
    let id = Uuid::new_v4();
    json!({"root":file.root,"door":"queue","origin":{"kind":"engine"},"core":{"plugin":"engine","tier":"core"},"uuid":id,
        "result":{"uuid":id,"message":{"type":"user","role":"user","content":[{"type":"text","text":format!("native wrapper\n{}\n",file.notice().unwrap())}]}}})
}
#[test]
fn native_whole_append_and_core_queue_are_distinct_acceptance_routes() {
    for addressed in [false, true] {
        let file = file(addressed);
        let result = if addressed {
            NativeInputResult::Queued(queued(&file))
        } else {
            NativeInputResult::Append(append(&file))
        };
        let receipt = InputReceipt::from_native(&file, result).unwrap();
        assert!(receipt.accepted);
        assert!(receipt.matches(&file).unwrap());
        let wrong = if addressed {
            NativeInputResult::Append(append(&file))
        } else {
            NativeInputResult::Queued(queued(&file))
        };
        assert!(InputReceipt::from_native(&file, wrong).is_err());
        let rejected = InputReceipt::from_native(&file, NativeInputResult::Failed).unwrap();
        assert!(!rejected.accepted);
        assert!(rejected.matches(&file).unwrap());
    }
}
#[test]
fn partial_foreign_wrong_door_or_unkept_queue_results_cannot_acknowledge() {
    let file = file(true);
    for mode in [
        "partial", "foreign", "door", "root", "origin", "uuid", "role", "twice", "nontext",
    ] {
        let mut result = queued(&file);
        match mode {
            "partial" => result["result"]["message"]["content"][0]["text"] = "tail only".into(),
            "foreign" => result["core"]["plugin"] = "foreign".into(),
            "door" => result["door"] = "delivery".into(),
            "root" => result["root"] = Uuid::new_v4().to_string().into(),
            "origin" => result["origin"]["kind"] = "task-notification".into(),
            "uuid" => result["result"]["uuid"] = Uuid::new_v4().to_string().into(),
            "role" => result["result"]["message"]["role"] = "assistant".into(),
            "twice" => {
                result["result"]["message"]["content"][0]["text"] =
                    file.notice().unwrap().repeat(2).into()
            }
            "nontext" => result["result"]["message"]["content"][0]["type"] = "image".into(),
            _ => unreachable!(),
        }
        assert!(
            InputReceipt::from_native(&file, NativeInputResult::Queued(result)).is_err(),
            "{mode}"
        );
    }
    assert!(
        serde_json::from_value::<NativeInputResult>(
            json!({"route":"prompt","result":{"text":file.notice().unwrap()}})
        )
        .is_err()
    );
}
#[test]
fn persisted_result_is_immutable_and_cannot_be_reused_for_another_head_or_body() {
    let root = tempfile::tempdir().unwrap();
    use std::os::unix::fs::DirBuilderExt;
    let home = root.path().canonicalize().unwrap().join("state");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let publisher = metadata::Publisher::new(home.clone());
    let file = file(true);
    file.publish(&publisher, &home).unwrap();
    let pointer = PendingInput::new(&file, 1).unwrap();
    pointer.publish(&publisher, &home).unwrap();
    let receipt =
        InputReceipt::from_native(&file, NativeInputResult::Queued(queued(&file))).unwrap();
    receipt.publish(&publisher, &home).unwrap();
    receipt.publish(&publisher, &home).unwrap();
    assert_eq!(
        InputReceipt::read(&InputReceipt::path(&home, file.context_id, file.token)).unwrap(),
        receipt
    );
    let mut successor = file.clone();
    successor.token = Uuid::new_v4();
    assert!(!receipt.matches(&successor).unwrap());
    assert!(!pointer.matches(&successor));
    successor = file.clone();
    let mut message = file.message().unwrap();
    message.body = "changed body".into();
    successor.text = message.text().unwrap();
    assert!(!receipt.matches(&successor).unwrap());
    let failed = InputReceipt::from_native(&file, NativeInputResult::Failed).unwrap();
    assert!(failed.publish(&publisher, &home).is_err());
    assert_eq!(pointer.input(&home).unwrap(), file);
    assert!(!failed.remove(&home).unwrap());
    assert!(receipt.remove(&home).unwrap());
    assert!(file.remove(&home).unwrap());
    assert!(pointer.remove(&home).unwrap());
}

#[test]
fn scope_cleanup_handles_each_partial_prefix_and_preserves_a_successor_scope() {
    for missing in ["none", "receipt", "payload", "both"] {
        let root = tempfile::tempdir().unwrap();
        use std::os::unix::fs::DirBuilderExt;
        let home = root.path().canonicalize().unwrap().join("state");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let publisher = metadata::Publisher::new(home.clone());
        let file = file(false);
        file.publish(&publisher, &home).unwrap();
        let pointer = PendingInput::new(&file, 1).unwrap();
        pointer.publish(&publisher, &home).unwrap();
        let receipt =
            InputReceipt::from_native(&file, NativeInputResult::Append(append(&file))).unwrap();
        receipt.publish(&publisher, &home).unwrap();
        let scope = crate::claude::NativeRegistration {
            owner_root: Uuid::new_v4(),
            observer: "physical-test-only".into(),
            generation: file.native_generation,
            root: file.root,
            context: Context {
                context_id: file.context_id,
                incarnation: file.incarnation,
                project_id: file.message().unwrap().project_id,
                host: HostKind::Claude,
                host_session_id: file.root.to_string(),
                host_version: "2.1.302".into(),
                os_profile: "test-only".into(),
                working_directory: "/test-owned/workspace".into(),
            },
        };
        let mut wrong = scope.clone();
        wrong.generation += 1;
        assert!(remove_scope(&home, &wrong).is_err());
        assert!(file.path(&home).exists());
        if missing == "receipt" || missing == "both" {
            receipt.remove(&home).unwrap();
        }
        if missing == "payload" || missing == "both" {
            file.remove(&home).unwrap();
        }
        remove_scope(&home, &scope).unwrap();
        assert_eq!(remove_scope(&home, &scope).unwrap(), 0);
        assert!(!file.path(&home).exists());
        assert!(!InputReceipt::path(&home, file.context_id, file.token).exists());
        assert!(!PendingInput::path(&home, file.context_id).exists());
    }
}

#[test]
fn legacy_negative_cannot_grant_retry_but_v2_refusal_can() {
    let file = file(false);
    let old = InputReceipt::from_native(&file, NativeInputResult::Failed).unwrap();
    assert_eq!(serde_json::to_value(&old).unwrap()["version"], 1);
    assert!(!old.accepted);
    assert!(!old.rejected_before_entry());
    for result in [
        NativeInputResult::RejectedBeforeEntry,
        NativeInputResult::Append(json!({"deny":"explicit native refusal"})),
    ] {
        let refused = InputReceipt::from_native(&file, result).unwrap();
        assert!(!refused.accepted);
        assert!(refused.rejected_before_entry());
        assert!(refused.matches(&file).unwrap());
        let decoded: InputReceipt =
            serde_json::from_value(serde_json::to_value(&refused).unwrap()).unwrap();
        assert!(decoded.rejected_before_entry());
    }
    let mut accepted = serde_json::to_value(
        InputReceipt::from_native(&file, NativeInputResult::Append(append(&file))).unwrap(),
    )
    .unwrap();
    accepted["version"] = 1.into();
    let old_positive: InputReceipt = serde_json::from_value(accepted).unwrap();
    assert!(old_positive.accepted && old_positive.matches(&file).unwrap());
}

#[test]
fn frozen_read_policy_is_part_of_the_exact_accepted_frame_digest() {
    for addressed in [false, true] {
        for auto_read in [false, true] {
            let mut file = file(addressed);
            let mut message = file.message().unwrap();
            message.auto_read = auto_read;
            file.text = message.text().unwrap();
            let native = if addressed {
                NativeInputResult::Queued(queued(&file))
            } else {
                NativeInputResult::Append(append(&file))
            };
            let receipt = InputReceipt::from_native(&file, native).unwrap();
            assert!(receipt.accepted && receipt.matches(&file).unwrap());
            let mut changed = file.clone();
            let mut input = changed.message().unwrap();
            input.auto_read = !auto_read;
            changed.text = input.text().unwrap();
            assert!(!receipt.matches(&changed).unwrap());
            let original_native = if addressed {
                NativeInputResult::Queued(queued(&file))
            } else {
                NativeInputResult::Append(append(&file))
            };
            assert!(InputReceipt::from_native(&changed, original_native).is_err());
        }
    }
}
