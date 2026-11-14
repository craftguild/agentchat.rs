//! Configuration preserves omission/false and snapshots cannot track later edits.

use agentchat_core::{
    ErrorCode,
    model::{ActorSelector, HostKind},
    project::{ProjectReceiptSettings, ReceiptSettingsUpdate},
};
use serde_json::json;

#[test]
fn null_and_omitted_updates_normalize_but_explicit_false_survives() {
    let omitted: ReceiptSettingsUpdate = serde_json::from_value(json!({})).unwrap();
    let nulls: ReceiptSettingsUpdate = serde_json::from_value(json!({
        "auto_read": null,
        "delivery_confirmation_timeout_seconds": null,
    }))
    .unwrap();
    assert_eq!(omitted, nulls);
    assert_eq!(serde_json::to_value(nulls).unwrap(), json!({}));
    let configured = ReceiptSettingsUpdate {
        auto_read: Some(false),
        delivery_confirmation_timeout_seconds: Some(1),
    }
    .apply(ProjectReceiptSettings::default())
    .unwrap();
    assert!(!configured.auto_read);
    assert_eq!(omitted.apply(configured).unwrap(), configured);
    let snapshot = configured.input_policy();
    let next = ReceiptSettingsUpdate {
        auto_read: Some(true),
        ..Default::default()
    }
    .apply(configured)
    .unwrap();
    assert!(!snapshot.auto_read && next.input_policy().auto_read);
}

#[test]
fn timeout_bounds_and_closed_fields_are_enforced_without_coercion() {
    for seconds in [0, 2_592_001, u32::MAX] {
        let update = ReceiptSettingsUpdate {
            delivery_confirmation_timeout_seconds: Some(seconds),
            ..Default::default()
        };
        assert_eq!(
            update.validate().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    for seconds in [1, 60, 2_592_000] {
        assert!(
            ReceiptSettingsUpdate {
                delivery_confirmation_timeout_seconds: Some(seconds),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }
    for bad in [
        json!({"auto_read":"false"}),
        json!({"auto_ack":0}),
        json!({"auto_ack":true}),
        json!({"auto_ack":null}),
        json!({"delivery_confirmation_timeout_seconds":1.5}),
        json!({"receipt_notices":true}),
    ] {
        assert!(serde_json::from_value::<ReceiptSettingsUpdate>(bad).is_err());
    }
}

#[test]
fn native_selectors_preserve_exact_host_and_opaque_chat_identity() {
    let chat_id = "case-Sensitive / chat 🔎".to_owned();
    for host in [HostKind::Codex, HostKind::Claude] {
        let selector = ActorSelector::Native {
            host,
            chat_id: chat_id.clone(),
        };
        assert!(selector.validate().is_ok());
        assert_eq!(
            serde_json::from_value::<ActorSelector>(serde_json::to_value(&selector).unwrap())
                .unwrap(),
            selector
        );
    }
    for chat_id in [String::new(), "é".repeat(513)] {
        assert_eq!(
            ActorSelector::Native {
                host: HostKind::Codex,
                chat_id
            }
            .validate()
            .unwrap_err()
            .code,
            ErrorCode::InvalidArgument
        );
    }
    assert!(
        serde_json::from_value::<ActorSelector>(
            json!({"kind":"native","host":"Codex","chat_id":"x"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActorSelector>(json!({"kind":"user","host":"codex"})).is_err()
    );
}

#[test]
fn fresh_defaults_have_one_hour_processing_timeout_and_only_automatic_read_policy() {
    let settings = ProjectReceiptSettings::default();
    assert_eq!(
        serde_json::to_value(settings).unwrap(),
        json!({"auto_read":true,"delivery_confirmation_timeout_seconds":3600})
    );
    assert_eq!(
        serde_json::to_value(settings.input_policy()).unwrap(),
        json!({"auto_read":true})
    );
}

#[test]
fn native_revision_accepts_only_the_explicit_dispatch_contract() {
    use agentchat_core::project::NativeFrameRevision;
    let revision = NativeFrameRevision::DispatchV4;
    assert_eq!(revision.code(), 4);
    assert_eq!(NativeFrameRevision::from_code(4).unwrap(), revision);
    assert_eq!(serde_json::to_value(revision).unwrap(), "dispatch_v4");
    for code in [i32::MIN, 0, 1, 2, 3, 5, i32::MAX] {
        assert!(NativeFrameRevision::from_code(code).is_err());
    }
    for old in ["legacy_v1", "receipt_policy_v2", "read_policy_v3"] {
        assert!(serde_json::from_value::<NativeFrameRevision>(json!(old)).is_err());
    }
}
