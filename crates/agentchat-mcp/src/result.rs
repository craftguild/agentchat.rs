//! One bounded structured envelope, without duplicating large bodies as text.

use agentchat_core::ErrorCode;
use agentchat_protocol::envelope::{Envelope, Outcome};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use serde_json::{Map, Value, json};
use std::sync::{Arc, OnceLock};

pub(super) fn schema() -> Arc<JsonObject> {
    static SCHEMA: OnceLock<Arc<JsonObject>> = OnceLock::new();
    SCHEMA.get_or_init(||Arc::new(json!({
        "type":"object","additionalProperties":false,
        "required":["version","request_id","project_id"],
        "properties":{
            "version":{"type":"integer","const":1},
            "request_id":{"type":"string","format":"uuid"},
            "project_id":{"type":["string","null"],"format":"uuid"},
            "result":{"type":"object","description":"The complete CLI-compatible domain result, including finite page cursors when applicable."},
            "error":{"type":"object","additionalProperties":false,"required":["code","message","retryable","details"],
                "properties":{"code":{"type":"string"},"message":{"type":"string"},"retryable":{"type":"boolean"},"details":{"type":"object"}}}
        },
        "oneOf":[{"required":["result"],"not":{"required":["error"]}},{"required":["error"],"not":{"required":["result"]}}]
    }).as_object().expect("static object schema").clone())).clone()
}
pub(super) fn wrap(envelope: Envelope<Value>) -> CallToolResult {
    let is_error = matches!(envelope.outcome, Outcome::Failure { .. });
    let id = envelope.request_id;
    let text = match &envelope.outcome {
        Outcome::Failure { error } => {
            // Some real hosts show only text for isError responses. Keep the
            // actionable code/retry identity visible without duplicating large
            // structured details or splitting a UTF-8 character.
            let brief = error.message.chars().take(512).collect::<String>();
            let mut summary = json!({
                "code":error.code,
                "message":brief,
                "retryable":error.retryable,
                "request_id":id,
            });
            if error.code == ErrorCode::UnsupportedProtocol
                && let Some(details) = compatibility_details(&error.details)
            {
                summary["compatibility_details"] = details;
            }
            format!(
                "agentchat error summary: {summary}. Retain request_id when retrying; complete details are in structuredContent."
            )
        }
        Outcome::Success { .. } => {
            format!("Complete agentchat result is in structuredContent; request_id={id}.")
        }
    };
    let value = serde_json::to_value(envelope).expect("JSON public envelope");
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    // Duplicating a 768 KiB page as escaped text can exceed the 1 MiB frame.
    // The complete envelope appears once; clients must consume structuredContent.
    result.structured_content = Some(value);
    result.is_error = Some(is_error);
    result
}

fn compatibility_details(details: &Value) -> Option<Value> {
    let object = details.as_object()?;
    let mut selected = Map::new();
    for key in ["host", "host_version", "observed_version", "missing_api"] {
        if let Some(value) = object.get(key) {
            selected.insert(key.into(), value.clone());
        }
    }
    (!selected.is_empty()).then_some(Value::Object(selected))
}
