//! Initialization error wrapping keeps compatibility details caller-visible.

use super::installation_failure;
use agentchat_core::{Error, ErrorCode};
use serde_json::json;

#[test]
fn unsupported_protocol_keeps_its_cause_and_promotes_skill_visible_details() {
    let mut error = Error::new(
        ErrorCode::UnsupportedProtocol,
        "The shared Codex server does not provide the required API surface.",
    );
    error.details = json!({
        "host":"codex",
        "host_version":"0.164.7",
        "observed_version":null,
        "missing_api":"initialize",
        "api_error_code":-32601,
    });
    let wrapped = installation_failure(
        error,
        &json!({"project_id":"00000000-0000-0000-0000-000000000001"}),
    );
    assert_eq!(wrapped.details["host"], "codex");
    assert_eq!(wrapped.details["host_version"], "0.164.7");
    assert!(wrapped.details["observed_version"].is_null());
    assert_eq!(wrapped.details["missing_api"], "initialize");
    assert_eq!(wrapped.details["cause"]["api_error_code"], -32601);
    assert_eq!(wrapped.details["integrations"]["state"], "failed");
}

#[test]
fn unrelated_installation_errors_remain_nested_without_compatibility_aliases() {
    let mut error = Error::new(ErrorCode::ConfigurationConflict, "collision");
    error.details = json!({"reason":"foreign_owner","host":"claude"});
    let wrapped = installation_failure(error, &json!({"created":true}));
    assert!(wrapped.details.get("host").is_none());
    assert_eq!(wrapped.details["cause"]["host"], "claude");
    assert_eq!(wrapped.details["initialization"]["created"], true);
}
