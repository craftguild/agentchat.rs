//! Complete preimages are durable before replacement; recovery never rolls back foreign edits.
use super::{InstallStage, Installer, Selector, conflict, fileio, strict_json};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Owned {
    pub file: Vec<u8>,
    pub selector: Selector,
    pub hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ledger {
    pub version: u32,
    pub roots: Vec<Vec<u8>>,
    pub entries: BTreeMap<String, Owned>,
}
impl Ledger {
    pub fn new(installer: &Installer) -> Self {
        Self {
            version: 1,
            roots: roots(installer),
            entries: BTreeMap::new(),
        }
    }
    pub fn validate(&self, installer: &Installer) -> Result<()> {
        if self.version != 1 || self.roots != roots(installer) || self.entries.len() > 128 {
            return Err(conflict(
                "Integration ownership format, roots or entry count is unsupported.",
            ));
        }
        let size = serde_json::to_vec_pretty(self)
            .map_err(|_| conflict("Cannot encode integration ownership."))?
            .len()
            + 1;
        if size > fileio::FILE_LIMIT {
            return Err(conflict("Integration ownership exceeds one MiB."));
        }
        for owned in self.entries.values() {
            destination(installer, &fileio::path(&owned.file)?)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Write {
    pub file: Vec<u8>,
    pub before: Option<String>,
    pub after: String,
    pub mode: u32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub version: u32,
    pub id: Uuid,
    pub before_ledger: Option<String>,
    pub ledger: Ledger,
    pub ownership_changed: bool,
    pub writes: Vec<Write>,
}
pub(super) fn roots(installer: &Installer) -> Vec<Vec<u8>> {
    installer.roots.iter().map(|p| fileio::native(p)).collect()
}
pub(super) fn destination(installer: &Installer, path: &Path) -> Result<()> {
    fileio::safe_parents(path)?;
    if path.starts_with(&installer.state)
        || !installer
            .roots
            .iter()
            .any(|root| path != root && path.starts_with(root))
    {
        return Err(conflict(
            "Integration destination is outside its selected host roots.",
        ));
    }
    Ok(())
}
pub(super) fn ledger(installer: &Installer) -> Result<(Option<String>, Ledger)> {
    let text = fileio::read(&installer.state.join("ownership.json"), fileio::FILE_LIMIT)?;
    let ledger = if let Some(text) = &text {
        serde_json::from_value(strict_json::parse(text)?)
            .map_err(|_| conflict("Invalid integration ownership record."))?
    } else {
        Ledger::new(installer)
    };
    ledger.validate(installer)?;
    Ok((text, ledger))
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|_| conflict("Cannot encode installation intent."))?;
    text.push('\n');
    if text.len() > fileio::JOURNAL_LIMIT {
        return Err(conflict("Installation intent exceeds its bounded size."));
    }
    Ok(text)
}
pub(super) fn begin(installer: &Installer, plan: &Plan) -> Result<()> {
    fileio::atomic(&installer.state.join("pending.json"), &encode(plan)?, 0o600)
}
pub(super) fn recover(installer: &Installer) -> Result<bool> {
    let Some(text) = fileio::read(&installer.state.join("pending.json"), fileio::JOURNAL_LIMIT)?
    else {
        return Ok(false);
    };
    let plan: Plan = serde_json::from_value(strict_json::parse(&text)?)
        .map_err(|_| conflict("Invalid installation recovery journal."))?;
    apply(installer, plan)?;
    Ok(true)
}
pub(super) fn apply(installer: &Installer, plan: Plan) -> Result<()> {
    if plan.version != 1 || plan.writes.len() > 16 {
        return Err(conflict("Unsupported installation recovery plan."));
    }
    plan.ledger.validate(installer)?;
    let ledger_path = installer.state.join("ownership.json");
    let new_ledger = encode(&plan.ledger)?;
    let actual_ledger = fileio::read(&ledger_path, fileio::FILE_LIMIT)?;
    if actual_ledger != plan.before_ledger && actual_ledger.as_deref() != Some(&new_ledger) {
        return Err(conflict(
            "Ownership record changed after installation intent.",
        ));
    }
    let mut total = 0usize;
    // Validate every destination before the first replacement, including during
    // recovery. A host's intervening edit must never be replaced by a preimage.
    for write in &plan.writes {
        let path = fileio::path(&write.file)?;
        destination(installer, &path)?;
        if write.after.len() > fileio::FILE_LIMIT
            || write
                .before
                .as_ref()
                .is_some_and(|s| s.len() > fileio::FILE_LIMIT)
            || write.mode & !0o777 != 0
        {
            return Err(conflict("Invalid bounded installation write."));
        }
        total += write.after.len() + write.before.as_ref().map_or(0, String::len);
        let actual = fileio::read(&path, fileio::FILE_LIMIT)?;
        if actual != write.before && actual.as_deref() != Some(&write.after) {
            return Err(conflict(
                "Host configuration changed after installation intent; recovery preserved it.",
            ));
        }
        if actual.is_some() && fileio::mode(&path)? != write.mode {
            return Err(conflict(
                "Host configuration permissions changed during installation.",
            ));
        }
    }
    if total > 4 * fileio::FILE_LIMIT {
        return Err(conflict("Installation write set exceeds four MiB."));
    }
    for (index, write) in plan.writes.iter().enumerate() {
        let path = fileio::path(&write.file)?;
        let stage = path
            .parent()
            .ok_or_else(|| conflict("Missing integration parent."))?
            .join(format!(".agentchat-install-{}-{index}", plan.id));
        // The durable journal owns this unpredictable exact staging filename.
        // A crash can leave partial bytes there; recovery removes only that file.
        fileio::publish(&path, &stage, &write.after, write.mode, Some(&write.before))?;
        installer.progress.reached(InstallStage::Published(index))?;
    }
    // Retain only the most recent complete backup, keeping disk use bounded.
    // It contains sensitive host config and is always private, never logged.
    fileio::atomic(
        &installer.state.join("last-backup.json"),
        &encode(&plan)?,
        0o600,
    )?;
    fileio::atomic(&ledger_path, &new_ledger, 0o600)?;
    installer
        .progress
        .reached(InstallStage::OwnershipPublished)?;
    fileio::remove(&installer.state.join("pending.json"))?;
    installer.progress.reached(InstallStage::Complete)
}
