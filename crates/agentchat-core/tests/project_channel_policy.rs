//! Ordered nonempty project defaults are constructed only through validation.

use agentchat_core::{ErrorCode, project::ProjectChannelPolicy};

#[test]
fn normalization_preserves_first_occurrence_order_and_primary() {
    let policy =
        ProjectChannelPolicy::new(vec![" Ops ".into(), "general".into(), "OPS".into()]).unwrap();
    assert_eq!(policy.ordered_defaults(), ["ops", "general"]);
    assert_eq!(policy.primary(), "ops");
    assert!(policy.contains("general"));
    assert!(!policy.contains("dev"));
}

#[test]
fn configured_defaults_cannot_be_empty_or_contain_empty_entries() {
    assert_eq!(
        ProjectChannelPolicy::new(Vec::new()).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        ProjectChannelPolicy::new(vec!["dev".into(), " ".into()])
            .unwrap_err()
            .code,
        ErrorCode::InvalidName
    );
}

#[test]
fn policy_has_no_unchecked_deserialization_boundary() {
    let serialized = serde_json::to_value(ProjectChannelPolicy::legacy()).unwrap();
    assert_eq!(serialized["ordered_defaults"], serde_json::json!(["dev"]));
    assert!(
        serde_json::from_value::<ProjectChannelPolicy>(serde_json::json!({"ordered_defaults":[]}))
            .is_err()
    );
}
