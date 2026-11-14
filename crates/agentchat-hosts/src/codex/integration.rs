//! Current scoped runtime/catalog metadata, never a successful model tool call.
use super::invalid;
use agentchat_core::{Error, ErrorCode, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{os::unix::ffi::OsStrExt, path::Path};

/// Correlate the effective MCP state namespace without publishing its path.
/// This is configuration identity only; live native scope, participation and
/// current controls must still be checked separately.
pub fn integration_namespace(home: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(b"agentchat/codex-native-state/v1\0");
    hash.update(home.as_os_str().as_bytes());
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn decode(value: &Value, namespace: Option<&str>) -> Result<bool> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if data.len() > 1 || value.get("nextCursor").is_some_and(|v| !v.is_null()) {
        return Err(invalid());
    }
    let Some(server) = data.first() else {
        return Ok(false);
    };
    if server.get("name").and_then(Value::as_str) != Some("agentchat") {
        return Err(invalid());
    }
    match server.get("runtimeStatus").and_then(Value::as_str) {
        Some("connected") => {}
        None | Some("notStarted" | "starting") => return Ok(false),
        Some(status @ ("authenticationRequired" | "failed" | "cancelled" | "disabled")) => {
            // A retained native thread can cache a dead MCP after its process
            // was stopped. Waiting cannot repair that connection, and treating
            // it as startup progress masks the cause behind a proxy timeout.
            let mut error = Error::new(
                ErrorCode::ConfigurationConflict,
                format!(
                    "Codex's AgentChat MCP connection is {status}. Restore agentchat through Codex's native MCP controls, fully exit the affected CLI, then retry the managed launch."
                ),
            );
            error.details = serde_json::json!({
                "host":"codex", "reason":"native_mcp_runtime_unavailable",
                "runtime_status":status,
                "required_action":"native_mcp_reconnect_then_fresh_managed_launch",
            });
            return Err(error);
        }
        _ => return Err(invalid()),
    }
    if server.pointer("/serverInfo/name").and_then(Value::as_str) != Some("agentchat")
        || server
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            != Some(env!("CARGO_PKG_VERSION"))
        || server.get("toolsError").is_some_and(|v| !v.is_null())
    {
        return Ok(false);
    }
    if namespace.is_some_and(|expected| {
        server
            .pointer("/serverCapabilities/experimental/agentchat~1native_routing/namespace")
            .and_then(Value::as_str)
            != Some(expected)
    }) {
        return Ok(false);
    }
    let routing_host = server
        .pointer("/serverCapabilities/experimental/agentchat~1native_routing/host")
        .and_then(Value::as_str);
    let protocol = server
        .pointer("/serverCapabilities/experimental/agentchat~1native_routing/protocol")
        .and_then(Value::as_u64);
    if routing_host == Some("codex")
        && protocol.is_some_and(|p| p != u64::from(agentchat_protocol::NATIVE_ROUTING_PROTOCOL))
    {
        // Native thread caches may retain the old physical helper after its CLI
        // exits. A matching package version/catalog cannot prove that helper
        // understands the new authority model. Never silently adopt it or
        // reconnect another frontend's tools as a startup side effect.
        let mut error = Error::new(
            ErrorCode::ConfigurationConflict,
            "Codex retains an incompatible AgentChat MCP connection. Reconnect agentchat through the native MCP controls, exit the CLI completely, then retry the managed launch.",
        );
        error.details = serde_json::json!({
            "host":"codex", "reason":"native_mcp_routing_epoch_mismatch",
            "expected_protocol":agentchat_protocol::NATIVE_ROUTING_PROTOCOL, "observed_protocol":protocol,
            "required_action":"native_mcp_reconnect_then_fresh_managed_launch",
        });
        return Err(error);
    }
    if routing_host != Some("codex")
        || protocol != Some(u64::from(agentchat_protocol::NATIVE_ROUTING_PROTOCOL))
    {
        return Ok(false);
    }
    let tools = server
        .get("tools")
        .and_then(Value::as_object)
        .ok_or_else(invalid)?;
    Ok(["agentchat_whoami", "agentchat_post", "agentchat_read"]
        .into_iter()
        .all(|name| {
            tools
                .get(name)
                .is_some_and(|v| v.get("name").and_then(Value::as_str) == Some(name))
        }))
}

#[cfg(test)]
#[path = "../../tests/unit/codex_integration.rs"]
mod tests;
