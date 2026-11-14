//! Completed init retries observe their original report without rewinding setup.

use super::{Artifact, Installer, conflict, current::Installed, fileio, journal, strict_json};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    request: Uuid,
    namespace: Uuid,
    hosts: Vec<HostKind>,
    #[serde(default)]
    report: Option<Value>,
}
impl Installer {
    /// Read a completed response before discovery. Legacy receipts cannot prove
    /// their original component report and must never be recomputed from today.
    pub(crate) async fn completed_request(
        &self,
        request: Uuid,
        namespace: Uuid,
    ) -> Result<Option<Value>> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _fence = this.lifecycle_fence()?;
            let _lock = fileio::lock(&this.state)?;
            read_receipt(&this.state, request, namespace)
        })
        .await
        .map_err(|_| Error::new(ErrorCode::IoError, "Installation worker failed."))?
    }
    /// The caller must validate the project's normalized actor/arguments first.
    /// The namespace fences accidental cross-project reuse of a completed key.
    pub(crate) async fn install_request<F, R>(
        &self,
        request: Uuid,
        namespace: Uuid,
        plan: F,
        report: R,
    ) -> Result<Value>
    where
        F: FnOnce(&Installed<'_>) -> Result<(Vec<Artifact>, Vec<HostKind>)> + Send + 'static,
        R: FnOnce(&Installed<'_>, &[HostKind]) -> Result<Value> + Send + 'static,
    {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _fence = this.lifecycle_fence()?;
            let _lock = fileio::lock(&this.state)?;
            let path = this.state.join("requests").join(format!("{request}.json"));
            if let Some(report) = read_receipt(&this.state, request, namespace)? {
                return Ok(report);
            }
            // Finish a pending publication before selecting the next mode.
            // Planning from its predecessor ledger could otherwise downgrade
            // a native bundle that recovery has just completed.
            journal::recover(&this)?;
            let installed = Installed::read(&this)?;
            let (artifacts, hosts) = plan(&installed)?;
            validate_hosts(&hosts)?;
            this.install_locked(artifacts)?;
            // Capture the complete result under the same installation lock,
            // after its owned ledger is durable. Later health is independent.
            let report = report(&Installed::read(&this)?, &hosts)?;
            validate_report(&report, &hosts)?;
            let receipt = Receipt {
                version: 2,
                request,
                namespace,
                hosts,
                report: Some(report.clone()),
            };
            let text = serde_json::to_string_pretty(&receipt)
                .map_err(|_| conflict("Cannot encode installation request receipt."))?
                + "\n";
            if text.len() > fileio::FILE_LIMIT {
                return Err(conflict("Installation request receipt exceeds one MiB."));
            }
            // Project creation and host-file publication are separate durable
            // operations. An interrupted installation resumes through its own
            // journal; this success receipt is published only after it completes.
            fileio::atomic(&path, &text, 0o600)?;
            Ok(report)
        })
        .await
        .map_err(|_| Error::new(ErrorCode::IoError, "Installation worker failed."))?
    }
}

fn read_receipt(state: &Path, request: Uuid, namespace: Uuid) -> Result<Option<Value>> {
    let path = state.join("requests").join(format!("{request}.json"));
    let Some(text) = fileio::read(&path, fileio::FILE_LIMIT)? else {
        return Ok(None);
    };
    let receipt: Receipt = serde_json::from_value(strict_json::parse(&text)?)
        .map_err(|_| conflict("Invalid installation request receipt."))?;
    if !matches!(receipt.version, 1 | 2) || receipt.request != request {
        return Err(conflict("Unsupported installation request receipt."));
    }
    if receipt.namespace != namespace {
        return Err(Error::new(
            ErrorCode::IdempotencyConflict,
            "Installation request belongs to a different project UUID.",
        ));
    }
    validate_hosts(&receipt.hosts)?;
    if receipt.version == 1 {
        let mut error = conflict(
            "The original installation result cannot be restored from this legacy receipt. Use status, or explicitly run ordinary agentchat install with a new request ID; do not automatically repeat --new-project or --relocate.",
        );
        error.details = json!({"reason":"historical_installation_result_unavailable",
            "receipt_version":1,"request_id":request});
        return Err(error);
    }
    let report = receipt
        .report
        .ok_or_else(|| conflict("Missing frozen installation result."))?;
    validate_report(&report, &receipt.hosts)?;
    // An earlier rename may have succeeded before its directory sync failed.
    fileio::durable(&path)?;
    Ok(Some(report))
}

fn validate_report(report: &Value, hosts: &[HostKind]) -> Result<()> {
    if !report.is_object()
        || report["state"]
            != if hosts.is_empty() {
                "not_installed"
            } else {
                "partial"
            }
        || !report["hosts"].is_object()
        || !report["lifecycle"].is_object()
    {
        return Err(conflict("Invalid frozen installation result."));
    }
    for (host, name) in [(HostKind::Codex, "codex"), (HostKind::Claude, "claude")] {
        let installed = hosts.contains(&host);
        if report["hosts"][name]["state"]
            != if installed {
                "tools_installed"
            } else {
                "not_installed"
            }
            || !report["hosts"][name]["components"].is_array()
        {
            return Err(conflict(
                "Frozen installation result differs from its host set.",
            ));
        }
    }
    Ok(())
}

fn validate_hosts(hosts: &[HostKind]) -> Result<()> {
    let canonical = [HostKind::Codex, HostKind::Claude]
        .into_iter()
        .filter(|host| hosts.contains(host))
        .collect::<Vec<_>>();
    if hosts != canonical {
        return Err(conflict(
            "Installation receipt requires a canonical unique host set.",
        ));
    }
    Ok(())
}
