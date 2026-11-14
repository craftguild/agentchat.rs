//! Delegate unavailable-server preparation to native idempotent start.
use super::{Daemon, discovery::output};
use agentchat_core::{Error, Result};
use std::{path::Path, time::Duration};

impl Daemon {
    /// Reuse a verified running server; otherwise ask the native CLI to ensure
    /// its ordinary shared daemon once. A failed probe is not absence proof:
    /// native idempotent start decides under its own lock whether to reuse or
    /// start. No restart/update/bootstrap, configuration override, model call
    /// or shared-daemon shutdown is performed. Command success is not readiness:
    /// independently inspect the real image/version afterwards. The caller
    /// separately verifies the live API before configuring a missing selector.
    pub async fn ensure_running(cli: &Path) -> Result<Self> {
        let initial_probe_error = match Self::inspect(cli).await {
            Ok(daemon) => return Ok(daemon),
            Err(error) => error,
        };
        // Native start serializes concurrent launchers. Cancellation may leave
        // its accepted shared-server side effect alive; never stop that server
        // or retry the command here to manufacture a successful return.
        tokio::time::timeout(
            Duration::from_secs(30),
            output(cli, &["app-server", "daemon", "start"]),
        )
        .await
        .map_err(|_| failure(super::unavailable(), initial_probe_error.clone(), "start"))?
        .map_err(|error| failure(error, initial_probe_error.clone(), "start"))?;
        Self::inspect(cli)
            .await
            .map_err(|error| failure(error, initial_probe_error, "reinspect"))
    }
}

fn failure(cause: Error, initial_probe_error: Error, stage: &str) -> Error {
    let mut error = Error::new(
        cause.code,
        "Codex shared server preparation did not complete. Start Codex normally and retry agentchat launch codex.",
    );
    error.retryable = cause.retryable;
    error.details = serde_json::json!({
        "host":"codex", "stage":stage,
        "required_command":if stage=="start" { "app-server daemon start" } else { "app-server daemon version" },
        "initial_probe_error":initial_probe_error, "cause":cause,
    });
    error
}
