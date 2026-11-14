//! Bounded physical allocation facts, independent of canonical host records.
use super::super::{
    record::{self, FileProof, Guard, Parent},
    unavailable,
};
use crate::claude::metadata::directory::{FileId, SocketId};
use crate::claude::process::ProcessIdentity;
use agentchat_core::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::staging) struct Allocation {
    pub root: Parent,
    pub guard: Option<Guard>,
    pub expected: Vec<String>,
    pub directory: Option<FileId>,
    pub sockets: Vec<(String, SocketId)>,
    pub endpoint: Option<FileProof>,
    pub native: Option<ProcessIdentity>,
}
impl Allocation {
    pub fn validate(&self) -> Result<()> {
        record::absolute(&record::path(&self.root.path))?;
        if let Some(path) = &self.root.alternate {
            record::absolute(&record::path(path))?;
        }
        if self.expected.is_empty()
            || self.expected.len() > 2
            || self.sockets.len() > self.expected.len()
            || self.directory.is_none() && !self.sockets.is_empty()
        {
            return Err(unavailable());
        }
        for (i, name) in self.expected.iter().enumerate() {
            record::component(name.as_bytes())?;
            if self.expected[..i].contains(name) {
                return Err(unavailable());
            }
        }
        for (i, (name, _)) in self.sockets.iter().enumerate() {
            if self.expected.get(i) != Some(name) {
                return Err(unavailable());
            }
        }
        if let Some(guard) = &self.guard {
            record::component(&guard.name)?;
        }
        Ok(())
    }
    pub fn extends(&self, old: &Self) -> bool {
        self.root == old.root
            && self.native == old.native
            && self.guard == old.guard
            && self.expected == old.expected
            && old.directory.is_none_or(|id| Some(id) == self.directory)
            && self.sockets.starts_with(&old.sockets)
            && old
                .endpoint
                .as_ref()
                .is_none_or(|proof| self.endpoint.as_ref() == Some(proof))
    }
}
