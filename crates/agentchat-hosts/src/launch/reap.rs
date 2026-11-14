//! Bounded original launch metadata collection after positive physical loss.
use super::{
    Binding, LaunchOwners, ReadyAuthorization, Registry, root_change::Changed, startup::Started,
};
use crate::claude::{
    metadata::{self, directory::Directory},
    process::BirthState,
};
use agentchat_core::{Error, ErrorCode, Result, model::LaunchOwnerState};
use std::{fs::ReadDir, path::PathBuf};
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct LaunchPage {
    pub visited: usize,
    pub removed: usize,
    pub preserved: usize,
}
struct Scan {
    directory: Directory,
    entries: Option<ReadDir>,
}
/// Retained small-file cursors compose into the existing maintenance worker.
/// No database/history scan, callback, native API, timer or directory deletion.
pub(crate) struct LaunchReaper {
    registry: Registry,
    scans: [Option<Scan>; 5],
    turn: usize,
}
impl LaunchReaper {
    pub fn new(home: PathBuf) -> Self {
        Self {
            registry: Registry::new(home),
            scans: [None, None, None, None, None],
            turn: 0,
        }
    }
    /// At most eight entries and eight original-file removals. A root departure
    /// stays durable while its original physical launch is current or unknown.
    /// Once native or supervisor birth loss is positive, the database can retire
    /// directly from its own immutable owner token without either routing file.
    pub fn step(&mut self) -> Result<LaunchPage> {
        let mut report = LaunchPage::default();
        let turn = self.turn;
        self.turn = (turn + 1) % 5;
        let kind = [
            "bindings",
            "root-changes",
            "started",
            "ready",
            "claude-profiles",
        ][turn];
        if self.scans[turn].is_none() {
            let Some(directory) = self.registry.data_directory(kind, false)? else {
                return Ok(report);
            };
            self.scans[turn] = Some(Scan {
                directory,
                entries: None,
            });
        }
        let scan = self.scans[turn].as_mut().expect("selected launch scan");
        scan.directory.current()?;
        if scan.entries.is_none() {
            scan.entries =
                Some(std::fs::read_dir(&scan.directory.path).map_err(|_| unavailable())?);
        }
        for _ in 0..8 {
            let Some(entry) = scan.entries.as_mut().and_then(Iterator::next) else {
                scan.entries = None;
                break;
            };
            report.visited += 1;
            let work = || -> Result<bool> {
                let name = entry.map_err(|_| unavailable())?.file_name();
                let key = name
                    .to_str()
                    .and_then(|n| n.strip_suffix(".json"))
                    .ok_or_else(unavailable)?;
                let uuid = || Uuid::parse_str(key).map_err(|_| unavailable());
                let snapshot = scan.directory.read(&name)?.ok_or_else(unavailable)?;
                if turn == 4 {
                    let selected: super::claude_profile::Selected =
                        metadata::decode_proved(&scan.directory, &name, &snapshot)?;
                    let owner = selected.owner()?;
                    if uuid()? != selected.identity.generation {
                        return Err(unavailable());
                    }
                    let _file = scan.directory.lock_record(&name, &snapshot)?;
                    // The snapshot also serves the historical native startup
                    // barrier. Supervisor loss alone must not erase it.
                    return if owner.birth_state() == BirthState::Gone {
                        scan.directory.remove_record(&name, &snapshot)
                    } else {
                        Ok(false)
                    };
                }
                let binding = if turn == 0 {
                    let binding: Binding =
                        metadata::decode_proved(&scan.directory, &name, &snapshot)?;
                    binding.validate()?;
                    if uuid()? != binding.context.context_id {
                        return Err(unavailable());
                    }
                    binding
                } else if turn == 1 {
                    let changed: Changed =
                        metadata::decode_proved(&scan.directory, &name, &snapshot)?;
                    if !changed.valid() || uuid()? != changed.binding.identity.generation {
                        return Err(unavailable());
                    }
                    changed.binding
                } else if turn == 2 {
                    let started: Started =
                        metadata::decode_proved(&scan.directory, &name, &snapshot)?;
                    if !started.valid() || uuid()? != started.binding.identity.generation {
                        return Err(unavailable());
                    }
                    started.binding
                } else {
                    let ready: ReadyAuthorization =
                        metadata::decode_proved(&scan.directory, &name, &snapshot)?;
                    if !ready.matches_key(key)? {
                        return Err(unavailable());
                    }
                    ready.binding().clone()
                };
                let _file = scan.directory.lock_record(&name, &snapshot)?;
                // A completed startup barrier must survive supervisor loss so
                // ordinary native input is not blocked after AGENTCHAT retires. It
                // carries no ongoing authority and waits for native birth loss.
                let lost = if turn >= 2 {
                    LaunchOwners::native_owner(&binding.identity)?.birth_state() == BirthState::Gone
                } else {
                    LaunchOwners::observe(&binding.identity) == LaunchOwnerState::Gone
                };
                if !lost {
                    return Ok(false);
                }
                scan.directory.remove_record(&name, &snapshot)
            };
            match work() {
                Ok(true) => report.removed += 1,
                Ok(false) | Err(_) => report.preserved += 1,
            }
        }
        Ok(report)
    }
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Original launch collection proof is unavailable or changed.",
    )
}

#[cfg(test)]
#[path = "../../tests/unit/launch_reaper.rs"]
mod tests;
