//! Install request normalization and durable effective-setting replay use real storage.

mod support;

use agentchat_core::{
    ErrorCode,
    project::{ProjectReceiptSettings, ReceiptSettingsUpdate},
};
use agentchat_runtime::project::InitRequest;
use serde_json::{Value, json};
use support::Fixture;

#[tokio::test]
async fn installation_preserves_omitted_settings_false_and_the_original_successful_snapshot() {
    let f = Fixture::new();
    let original = f.request();
    let first = f
        .manager
        .initialize(&f.workspace, original.clone())
        .await
        .unwrap();
    assert_eq!(
        first.receipt_settings,
        Some(ProjectReceiptSettings::default())
    );
    let mut changed = f.request();
    changed.receipt_settings = ReceiptSettingsUpdate {
        auto_read: Some(false),
        delivery_confirmation_timeout_seconds: Some(300),
    };
    let second = f
        .manager
        .initialize(&f.workspace, changed.clone())
        .await
        .unwrap();
    assert_eq!(
        second.receipt_settings,
        Some(ProjectReceiptSettings {
            auto_read: false,
            delivery_confirmation_timeout_seconds: 300,
        })
    );
    assert_eq!(
        f.manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap()
            .receipt_settings,
        second.receipt_settings
    );
    // Successful retry returns its original policy snapshot even after a later
    // administrative request changes effective settings. It never reapplies it.
    assert_eq!(
        f.manager.initialize(&f.workspace, original).await.unwrap(),
        first
    );
    assert_eq!(
        f.manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap()
            .receipt_settings,
        second.receipt_settings
    );
    let mut conflict = changed;
    conflict.receipt_settings.auto_read = Some(true);
    assert_eq!(
        f.manager
            .initialize(&f.workspace, conflict)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
}

#[test]
fn null_and_omitted_updates_share_retry_identity_but_explicit_false_is_retained() {
    let request = InitRequest::new(uuid::Uuid::new_v4());
    let mut raw = serde_json::to_value(&request).unwrap();
    assert!(raw.get("receipt_settings").is_none());
    raw["receipt_settings"] =
        json!({"auto_read":null,"delivery_confirmation_timeout_seconds":null});
    let normalized: InitRequest = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(normalized, request);
    assert_eq!(
        serde_json::to_value(normalized).unwrap(),
        serde_json::to_value(&request).unwrap()
    );
    raw["receipt_settings"]["auto_read"] = false.into();
    let explicit: InitRequest = serde_json::from_value(raw).unwrap();
    assert_ne!(explicit, request);
    assert_eq!(explicit.receipt_settings.auto_read, Some(false));
}

#[tokio::test]
async fn historical_completed_install_receipts_are_not_expanded_with_current_settings() {
    let f = Fixture::new();
    let request = f.request();
    let first = f
        .manager
        .initialize(&f.workspace, request.clone())
        .await
        .unwrap();
    let receipt = f
        .manager
        .home()
        .join("init/requests")
        .join(format!("{}.json", request.request_id));
    let mut historical: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    historical
        .as_object_mut()
        .unwrap()
        .remove("receipt_settings");
    historical["request"]["request"]
        .as_object_mut()
        .unwrap()
        .remove("receipt_settings");
    std::fs::write(&receipt, serde_json::to_vec(&historical).unwrap()).unwrap();
    let before = std::fs::read(&receipt).unwrap();
    let replay = f.manager.initialize(&f.workspace, request).await.unwrap();
    assert_eq!(replay.project, first.project);
    assert_eq!(replay.receipt_settings, None);
    assert_eq!(std::fs::read(&receipt).unwrap(), before);
}

#[tokio::test]
async fn invalid_setting_updates_fail_before_creating_project_or_state() {
    let f = Fixture::new();
    for seconds in [0, 2_592_001, u32::MAX] {
        let mut request = f.request();
        request
            .receipt_settings
            .delivery_confirmation_timeout_seconds = Some(seconds);
        assert_eq!(
            f.manager
                .initialize(&f.workspace, request)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    assert!(!f.manager.home().exists());
    assert!(!f.workspace.join(".agentchat").exists());
}

#[test]
fn fresh_receipt_settings_use_one_hour_and_reject_removed_ack_fields() {
    let settings = ProjectReceiptSettings::default();
    assert_eq!(settings.delivery_confirmation_timeout_seconds, 3_600);
    assert!(settings.auto_read);
    let value = serde_json::to_value(settings).unwrap();
    assert!(value.get("auto_ack").is_none());
    let mut removed = value;
    removed["auto_ack"] = true.into();
    assert!(serde_json::from_value::<ProjectReceiptSettings>(removed).is_err());
    assert!(serde_json::from_value::<ReceiptSettingsUpdate>(json!({"auto_ack":false})).is_err());
}
