//! Plain presentation preserves full printable bodies and contains terminal controls.

#[path = "../src/presentation.rs"]
mod presentation;
#[allow(dead_code)]
#[path = "../src/terminal.rs"]
mod terminal;

use agentchat_core::{Error, ErrorCode};
use agentchat_protocol::envelope::Envelope;
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn full_multiline_unicode_bodies_ids_and_cursors_are_readable_without_a_json_envelope() {
    let mut body = "  日本語 first\n".to_owned();
    let suffix = "\n trailing  \n";
    body.push_str(&"x".repeat(65_536 - body.len() - suffix.len()));
    body.push_str(suffix);
    assert_eq!(body.len(), 65_536);
    let id = Uuid::new_v4();
    let value = json!({"message":{"id":id,"body":body,"author":{"actor_id":id,"kind":"user","handle":"user"}},"receipts":{"items":[],"next_cursor":"9007199254740993","head":"9223372036854775807"}});
    let rendered = presentation::render(&Envelope::new(id, Some(id), Ok(value)));
    assert!(rendered.contains(&format!("Body ({} bytes):\n{body}", body.len())));
    assert!(rendered.contains("Next cursor: 9007199254740993"));
    assert!(rendered.contains("Head: 9223372036854775807"));
    assert!(rendered.contains(&format!("Actor id: {id}")));
    assert!(!rendered.contains("\"message\""));
    assert!(!rendered.contains("\"result\""));
    assert!(serde_json::from_str::<Value>(&rendered).is_err());
}

#[test]
fn every_nested_result_shape_is_plain_including_dynamic_diagnostics_and_terminal_controls() {
    let rendered = presentation::render(&Envelope::new(
        Uuid::new_v4(),
        None,
        Ok(json!({
            "body":"safe\n\u{001b}[31m\r\t\u{009b}\u{202e}\u{2066}尾\n",
            "integrations":{"components":[{"key":"native\u{001b}","state":"owned"}]},
            "counts":[0,true,null],"empty":{},"warnings":["unconfirmed"]
        })),
    ));
    assert!(
        rendered.contains("safe\n\\u{001b}[31m\\u{000d}\\u{0009}\\u{009b}\\u{202e}\\u{2066}尾\n")
    );
    assert!(rendered.contains("Components (1):\n"));
    assert!(rendered.contains("Key: native\\u{001b}"));
    assert!(!rendered.chars().any(|c| (c.is_control() && c != '\n')
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')));
    assert!(!rendered.contains("\"state\""));
}

#[test]
fn failures_retain_code_retry_identity_and_details_in_plain_text() {
    let id = Uuid::new_v4();
    let mut error = Error::new(
        ErrorCode::SessionRequired,
        "Native\u{001b}[2J authority missing",
    );
    error.details =
        json!({"reason":"managed_launch_required","required_command":"agentchat launch codex"});
    let output = presentation::render(&Envelope::new(id, None, Err(error)));
    assert!(output.starts_with(&format!("Request: {id}\nError: session_required\n")));
    assert!(output.contains("Message: Native\\u{001b}[2J authority missing\n"));
    assert!(output.contains("Required command: agentchat launch codex"));
    assert!(!output.contains("\"error\""));
}
