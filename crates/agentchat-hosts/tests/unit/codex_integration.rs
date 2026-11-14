use super::*;
use serde_json::json;

#[test]
fn terminal_native_mcp_states_require_native_recovery_without_false_readiness() {
    for state in ["failed", "cancelled", "disabled", "authenticationRequired"] {
        let value = json!({"data":[{"name":"agentchat","runtimeStatus":state}],"nextCursor":null});
        let error = decode(&value, Some("current-scope")).unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigurationConflict);
        assert!(!error.retryable);
        assert_eq!(error.details["host"], "codex");
        assert_eq!(error.details["reason"], "native_mcp_runtime_unavailable");
        assert_eq!(error.details["runtime_status"], state);
        assert_eq!(
            error.details["required_action"],
            "native_mcp_reconnect_then_fresh_managed_launch"
        );
        assert!(error.message.contains("native MCP controls"));
    }
}

#[test]
fn absent_and_inflight_native_mcp_states_remain_pending() {
    for state in [json!(null), json!("notStarted"), json!("starting")] {
        assert!(
            !decode(
                &json!({"data":[{"name":"agentchat","runtimeStatus":state}]}),
                None
            )
            .unwrap()
        );
    }
    assert!(!decode(&json!({"data":[]}), None).unwrap());
    assert!(
        decode(
            &json!({"data":[{"name":"agentchat","runtimeStatus":"unknown-shape"}]}),
            None
        )
        .is_err()
    );
}
