//! Exact host-component removal plans; application policy owns their lifetime.
mod artifacts;
mod files;
mod hooks;
mod publication;
mod recognize;
mod selectors;

use super::{Installer, Selector, conflict, fileio, journal};
use crate::{
    lifecycle::Fence,
    setup::{Environment, Roots},
};
use agentchat_core::{Error, Result, model::HostKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Secret-bearing recoverable host plan. Serialize only into a private removal
/// journal/backup; diagnostics must use component names and destinations only.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovalPlan {
    id: Uuid,
    state: Vec<u8>,
    edits: Vec<Edit>,
    directories: Vec<EmptyDirectory>,
    ownership: BTreeMap<String, journal::Owned>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    file: Vec<u8>,
    keys: Vec<String>,
    before: String,
    after: Option<String>,
    mode: u32,
    identity: (u64, u64),
    parent: (u64, u64),
    stage: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyDirectory {
    path: Vec<u8>,
    identity: (u64, u64),
    mode: u32,
    parent: (u64, u64),
}

/// Captured exact supported destinations and the preserving ownership namespace.
/// Construction does not execute native hosts or mutate configuration.
pub struct Remover {
    installer: Installer,
    roots: Roots,
    environment: Environment,
}
impl Remover {
    /// Resolve the same captured host-root rules as normal installation.
    pub fn new(state: &Path, environment: Environment) -> Result<Self> {
        let roots = environment.resolve()?;
        let installer = Installer::new(
            state,
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )?;
        Ok(Self {
            installer,
            roots,
            environment,
        })
    }
    fn state(&self) -> &Path {
        self.installer
            .state
            .parent()
            .expect("integration state root")
    }
    /// Exact supported file destinations, including absent components, used to
    /// bind administrative retry identity and reject overlapping deletion plans.
    pub fn destinations(&self) -> Result<Vec<PathBuf>> {
        selectors::destinations(self)
    }
    /// Validate all current components before any host write. Missing ledgers
    /// permit only bounded exact-selector/recognizable-content recovery.
    pub fn prepare(&self, id: Uuid, fence: &Fence) -> Result<RemovalPlan> {
        fence.require_exclusive(self.state())?;
        let (_, ledger) = journal::ledger(&self.installer).map_err(|e| {
            component(
                e,
                "integration.ownership",
                &self.installer.state.join("ownership.json"),
            )
        })?;
        selectors::validate_ledger(self, &ledger)?;
        let mut plan = RemovalPlan {
            id,
            state: fileio::native(self.state()),
            edits: Vec::new(),
            directories: Vec::new(),
            ownership: ledger.entries.clone(),
        };
        selectors::prepare(self, &ledger, &mut plan)?;
        self.validate(&plan, fence)?;
        Ok(plan)
    }
    /// Revalidate scope, exact destinations, bounds and every remaining preimage.
    pub fn validate(&self, plan: &RemovalPlan, fence: &Fence) -> Result<()> {
        files::validate(self, plan, fence)
    }
    /// Publish private preimages outside state/config deletion targets before
    /// the first host edit. Returns none when no host component needs mutation.
    pub fn backup(&self, plan: &RemovalPlan, fence: &Fence) -> Result<Option<PathBuf>> {
        files::backup(self, plan, fence)
    }
    /// Apply one bounded file/directory component idempotently. The caller
    /// durably records progress and retains its same exclusive lifecycle fence.
    pub fn apply(&self, plan: &RemovalPlan, index: usize, fence: &Fence) -> Result<()> {
        files::apply(self, plan, index, fence)
    }
}
impl RemovalPlan {
    /// Administrative request whose private recovery metadata owns this plan.
    pub fn request_id(&self) -> Uuid {
        self.id
    }
    /// Total ordered publication components, including exclusive empty dirs.
    pub fn len(&self) -> usize {
        self.edits.len() + self.directories.len()
    }
    /// Whether every supported component was already absent.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Safe bounded identifiers for outcomes, never preimage/configuration data.
    pub fn components(&self) -> Vec<String> {
        self.edits
            .iter()
            .map(|e| e.keys.join("+"))
            .chain(
                self.directories
                    .iter()
                    .map(|_| "integration.empty_directory".into()),
            )
            .collect()
    }
}

fn component(mut error: Error, key: &str, path: &Path) -> Error {
    error.details = serde_json::json!({"component":key,"destination":agentchat_protocol::removal::RemovalPath::new(path)});
    error
}
fn verify(
    ledger: &journal::Ledger,
    key: &str,
    path: &Path,
    selector: Selector,
    hash: String,
    recognizable: bool,
) -> Result<()> {
    let valid = if let Some(owned) = ledger.entries.get(key) {
        owned.file == fileio::native(path) && owned.selector == selector && owned.hash == hash
    } else {
        recognizable
    };
    if !valid {
        return Err(component(
            conflict("AgentChat removal preserved an ambiguous or externally changed component."),
            key,
            path,
        ));
    }
    Ok(())
}
fn json_text(value: &serde_json::Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)
        .map_err(|_| conflict("Cannot encode preserving host removal."))?
        + "\n")
}
