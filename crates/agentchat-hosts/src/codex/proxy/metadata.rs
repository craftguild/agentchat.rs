//! Borrowed lifecycle metadata; unrelated values never become owned JSON trees.

use crate::codex::{required, thread_id};
use agentchat_core::Result;
use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, Visitor},
};
use serde_json::value::RawValue;
use std::fmt;
use tokio_tungstenite::tungstenite::Message;

// Limits apply before decoding, including escaped strings. The root bound
// admits every JSON escape spelling of the existing 1024-byte root limit.
const ID_JSON_LIMIT: usize = 8192;
const ROOT_JSON_LIMIT: usize = 6 * 1024 + 2;

#[derive(Default)]
pub(super) struct Fields<'a> {
    id: Option<&'a RawValue>,
    method: Option<&'a RawValue>,
    params: Option<&'a RawValue>,
    result: Option<&'a RawValue>,
    error: Option<&'a RawValue>,
    ephemeral: Option<&'a RawValue>,
    thread_id: Option<&'a RawValue>,
    thread: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(field_identifier)]
enum Field {
    #[serde(rename = "id")]
    Id,
    #[serde(rename = "method")]
    Method,
    #[serde(rename = "params")]
    Params,
    #[serde(rename = "result")]
    Result,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "ephemeral")]
    Ephemeral,
    #[serde(rename = "threadId")]
    ThreadId,
    #[serde(rename = "thread")]
    Thread,
    #[serde(other)]
    Other,
}

impl<'de> Deserialize<'de> for Fields<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Metadata;
        impl<'de> Visitor<'de> for Metadata {
            type Value = Fields<'de>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an RPC metadata object")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut fields = Fields::default();
                while let Some(key) = map.next_key()? {
                    let slot = match key {
                        Field::Id => &mut fields.id,
                        Field::Method => &mut fields.method,
                        Field::Params => &mut fields.params,
                        Field::Result => &mut fields.result,
                        Field::Error => &mut fields.error,
                        Field::Ephemeral => &mut fields.ephemeral,
                        Field::ThreadId => &mut fields.thread_id,
                        Field::Thread => &mut fields.thread,
                        Field::Other => {
                            map.next_value::<IgnoredAny>()?;
                            continue;
                        }
                    };
                    // Preserve Value's last-key-wins behavior and distinguish
                    // an explicit null from an absent field without owning it.
                    *slot = Some(map.next_value()?);
                }
                Ok(fields)
            }
        }
        deserializer.deserialize_map(Metadata)
    }
}

impl Fields<'_> {
    pub(super) fn method(&self) -> Option<String> {
        text(self.method?, 128)
    }

    pub(super) fn request_id(&self) -> Option<String> {
        let raw = self.id?.get();
        if raw.len() > ID_JSON_LIMIT {
            return None;
        }
        // JSON-RPC IDs are scalars. Never deserialize an arbitrary compound ID
        // supplied by a prompt/tool/history-shaped value, even on rejection.
        match raw.as_bytes().first()? {
            b'"' => serde_json::to_string(&serde_json::from_str::<String>(raw).ok()?).ok(),
            b'-' | b'0'..=b'9' => serde_json::from_str::<serde_json::Number>(raw)
                .ok()
                .map(|id| id.to_string()),
            b'n' if raw == "null" => Some("null".into()),
            _ => None,
        }
    }
}

pub(super) fn envelope(message: &Message) -> Option<Fields<'_>> {
    let Message::Text(text) = message else {
        return None;
    };
    serde_json::from_str(text).ok()
}

fn object(raw: Option<&RawValue>) -> Fields<'_> {
    raw.and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or_default()
}

fn text(raw: &RawValue, limit: usize) -> Option<String> {
    (raw.get().len() <= limit)
        .then(|| serde_json::from_str(raw.get()).ok())
        .flatten()
}

fn root(raw: Option<&RawValue>) -> Option<String> {
    text(raw?, ROOT_JSON_LIMIT).filter(|root| thread_id(root))
}

pub(super) fn candidate_request(message: &Message) -> Result<Option<(String, Option<String>)>> {
    let Some(fields) = envelope(message) else {
        return Ok(None);
    };
    let Some(method) = fields.method() else {
        return Ok(None);
    };
    if !matches!(method.as_str(), "thread/start" | "thread/resume") {
        return Ok(None);
    }
    let params = object(fields.params);
    // Only the exact typed signal exempts internal structured model/tool work.
    if method == "thread/start" && params.ephemeral.is_some_and(|raw| raw.get() == "true") {
        return Ok(None);
    }
    let id = fields.request_id().ok_or_else(required)?;
    let expected_root = if method == "thread/resume" {
        Some(root(params.thread_id).ok_or_else(required)?)
    } else {
        None
    };
    Ok(Some((id, expected_root)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CandidateOutcome {
    Failed,
    Succeeded(String),
    Indeterminate,
}

pub(super) fn candidate_response(
    message: &Message,
    pending: impl FnOnce(&str) -> bool,
) -> Result<Option<(String, CandidateOutcome)>> {
    let Some(fields) = envelope(message) else {
        return Ok(None);
    };
    // The server may issue a request using the same scalar ID. Its independent
    // request namespace cannot satisfy a pending client transition response.
    if fields.method.is_some() {
        return Ok(None);
    }
    let Some(id) = fields.request_id() else {
        return Ok(None);
    };
    // Borrowing the raw result above does not parse its fields. Only a matching
    // foreground request may cause result-root/error interpretation.
    if !pending(&id) {
        return Ok(None);
    }
    if fields.error.is_some_and(|error| error.get() != "null") {
        return Ok(Some((id, CandidateOutcome::Failed)));
    }
    let result = object(fields.result);
    let thread = object(result.thread);
    let returned_root = root(thread.id.or(result.thread_id));
    Ok(Some((
        id,
        returned_root.map_or(CandidateOutcome::Indeterminate, CandidateOutcome::Succeeded),
    )))
}
