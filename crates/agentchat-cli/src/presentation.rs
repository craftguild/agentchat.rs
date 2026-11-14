//! Plain finite output; dynamic mutation/diagnostic fields never fall back to JSON.

use crate::terminal::escape;
use agentchat_protocol::envelope::{Envelope, Outcome};
use serde_json::Value;
use std::fmt::Write;

pub(crate) fn render(envelope: &Envelope<Value>) -> String {
    let mut output = format!("Request: {}\n", envelope.request_id);
    if let Some(project) = envelope.project_id {
        writeln!(output, "Project: {project}").expect("string output");
    }
    match &envelope.outcome {
        Outcome::Success { result } => match result {
            Value::Object(fields) if !fields.is_empty() => {
                for (key, value) in fields {
                    field(&mut output, key, value, 0);
                }
            }
            Value::Null => output.push_str("Success\n"),
            other => field(&mut output, "result", other, 0),
        },
        Outcome::Failure { error } => {
            let code = serde_json::to_value(error.code).expect("error code serialization");
            writeln!(
                output,
                "Error: {}",
                code.as_str().expect("error code spelling")
            )
            .expect("string output");
            writeln!(output, "Message: {}", escape(&error.message)).expect("string output");
            writeln!(output, "Retryable: {}", error.retryable).expect("string output");
            if !error.details.is_null() && error.details != serde_json::json!({}) {
                field(&mut output, "details", &error.details, 0);
            }
        }
    }
    output
}

fn field(output: &mut String, key: &str, value: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    let label = label(key);
    // A body remains one complete printable block, including literal newlines
    // and leading/trailing whitespace. Only hostile controls are escaped.
    if key == "body"
        && let Value::String(body) = value
    {
        writeln!(output, "{indent}{label} ({} bytes):", body.len()).expect("string output");
        output.push_str(&body.split('\n').map(escape).collect::<Vec<_>>().join("\n"));
        if !body.ends_with('\n') {
            output.push('\n');
        }
        return;
    }
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            writeln!(output, "{indent}{label}:").expect("string output");
            for (key, value) in fields {
                field(output, key, value, depth + 1);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            writeln!(output, "{indent}{label} ({}):", items.len()).expect("string output");
            for (index, value) in items.iter().enumerate() {
                field(output, &(index + 1).to_string(), value, depth + 1);
            }
        }
        Value::Array(_) | Value::Object(_) => {
            writeln!(output, "{indent}{label}: none").expect("string output");
        }
        Value::String(text) => {
            writeln!(output, "{indent}{label}: {}", escape(text)).expect("string output");
        }
        Value::Null => {
            writeln!(output, "{indent}{label}: none").expect("string output");
        }
        Value::Bool(value) => {
            writeln!(output, "{indent}{label}: {value}").expect("string output");
        }
        Value::Number(value) => {
            writeln!(output, "{indent}{label}: {value}").expect("string output");
        }
    }
}

fn label(key: &str) -> String {
    let text = key.replace('_', " ");
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        escape(&(first.to_uppercase().collect::<String>() + chars.as_str()))
    })
}
