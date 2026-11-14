//! Bounded read-only identity inspection; live APIs are checked when used.

use agentchat_core::model::HostKind;
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

#[cfg(test)]
#[path = "../tests/unit/diagnostics.rs"]
mod tests;

/// Inspect installed host identities without claiming live API compatibility.
pub async fn inspect() -> Value {
    let (codex, claude) = tokio::join!(
        version(HostKind::Codex, "codex"),
        version(HostKind::Claude, "claude")
    );
    json!({"codex":codex,"claude":claude})
}
async fn version(host: HostKind, program: &str) -> Value {
    let work = async {
        let mut child = Command::new(program)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut output = Vec::new();
        let mut stdout = child.stdout.take().expect("piped output").take(4097);
        let (read, status) = tokio::join!(stdout.read_to_end(&mut output), child.wait());
        read?;
        let status = status?;
        if !status.success() || output.len() > 4096 {
            return Ok::<_, std::io::Error>(
                json!({"state":"version_unavailable","api_surface":"unchecked"}),
            );
        }
        let version = String::from_utf8(output).ok().map(|s| s.trim().to_owned());
        Ok(observation(host, version))
    };
    match timeout(Duration::from_secs(2), work).await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            json!({"state":"missing","api_surface":"unchecked"})
        }
        _ => json!({"state":"version_unavailable","api_surface":"unchecked"}),
    }
}

fn observation(host: HostKind, version: Option<String>) -> Value {
    let exact = version.as_deref().and_then(|value| match host {
        HostKind::Codex => value.strip_prefix("codex-cli ")?.split_whitespace().next(),
        HostKind::Claude => value.split_whitespace().next(),
    });
    let candidate = exact.is_some_and(|version| {
        crate::require_native_profile(
            host,
            version,
            &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        )
        .is_ok()
    });
    json!({"state":if candidate {"candidate"} else {"version_unavailable"},
        "version":version,"api_surface":"checked_on_use","best_effort":candidate})
}
