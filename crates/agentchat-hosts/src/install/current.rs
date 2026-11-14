//! Read managed selectors while the caller holds its existing installer lock.
use super::{Installer, Selector, conflict, fileio, journal, merge, strict_json, value_digest};
use agentchat_core::Result;
use serde_json::Value;
use std::path::Path;

// Construct only inside a locked install or inspection operation. The view
// cannot outlive that operation and never accepts an unowned current value.
pub(crate) struct Installed<'a> {
    installer: &'a Installer,
    pub(super) ledger: journal::Ledger,
}
impl<'a> Installed<'a> {
    pub(super) fn read(installer: &'a Installer) -> Result<Self> {
        let (_, ledger) = journal::ledger(installer)?;
        Ok(Self { installer, ledger })
    }
    pub(crate) fn has(&self, key: &str) -> bool {
        self.ledger.entries.contains_key(key)
    }
    pub(crate) fn has_prefix(&self, prefix: &str) -> bool {
        self.ledger
            .entries
            .keys()
            .any(|key| key.starts_with(prefix))
    }
    pub(crate) fn json(&self, key: &str, file: &Path, path: &[&str]) -> Result<Option<Value>> {
        let Some(owned) = self.ledger.entries.get(key) else {
            return Ok(None);
        };
        let file = merge::destination(self.installer, file)?;
        let selector = Selector::Json {
            path: path.iter().map(|key| (*key).to_owned()).collect(),
        };
        if owned.file != fileio::native(&file) || owned.selector != selector {
            return Err(conflict(
                "Managed selection destination or selector changed.",
            ));
        }
        let text = fileio::read(&file, fileio::FILE_LIMIT)?
            .ok_or_else(|| conflict("Managed selection is missing."))?;
        let document = strict_json::parse(&text)?;
        let mut value = &document;
        for key in path {
            value = value
                .get(*key)
                .ok_or_else(|| conflict("Managed selection is missing."))?;
        }
        if value_digest(value)? != owned.hash {
            return Err(conflict("Managed selection changed outside its installer."));
        }
        Ok(Some(value.clone()))
    }
    pub(crate) fn toml(&self, key: &str, file: &Path, path: &[&str]) -> Result<Option<Value>> {
        let Some(owned) = self.ledger.entries.get(key) else {
            return Ok(None);
        };
        let file = merge::destination(self.installer, file)?;
        let selector = Selector::Toml {
            path: path.iter().map(|key| (*key).to_owned()).collect(),
        };
        if owned.file != fileio::native(&file) || owned.selector != selector {
            return Err(conflict(
                "Managed selection destination or selector changed.",
            ));
        }
        let text = fileio::read(&file, fileio::FILE_LIMIT)?
            .ok_or_else(|| conflict("Managed selection is missing."))?;
        let document = super::toml::parse(&text)?;
        let mut item = document.as_item();
        for key in path {
            item = item
                .as_table_like()
                .and_then(|table| table.get(key))
                .ok_or_else(|| conflict("Managed selection is missing."))?;
        }
        let value = super::toml::json(item)?;
        if value_digest(&value)? != owned.hash {
            return Err(conflict("Managed selection changed outside its installer."));
        }
        Ok(Some(value))
    }
}
