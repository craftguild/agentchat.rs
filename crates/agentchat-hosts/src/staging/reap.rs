//! Finite retained traversal of pending origins, never canonical application history.
use super::{
    ContextStageValidator, OriginRegistry, StagePurpose,
    record::{self, Record},
    unavailable,
};
use crate::claude::{
    metadata::directory::{Directory, FileId, Snapshot},
    process::BirthState,
};
use agentchat_core::Result;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::ReadDir,
    path::{Path, PathBuf},
};

/// One finite pending-origin pass. Counts describe physical I/O, never delivery.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OriginReport {
    /// All entries inspected, including invalid and live records; at most eight.
    pub visited: usize,
    /// Exact original stages and witnesses removed; at most sixteen.
    pub removed: usize,
    /// Original empty external directories removed; at most one per pass.
    pub directories_removed: usize,
    /// Live, uncertain, changed or role-specific state deliberately retained.
    pub preserved: usize,
    /// Incomplete, invalid or changed proof, included in `preserved`.
    pub ambiguous: usize,
    /// Complete orphan data awaiting its role-specific recovery protocol.
    pub pending_policy: usize,
}
/// A lazy synchronous collector with a retained directory cursor. It never
/// scans immutable canonical history, creates missing roots, initializes a
/// host, or treats an image change as physical process death.
pub struct OriginReaper {
    context: Option<ContextStageValidator>,
    registry: OriginRegistry,
    identity: Option<FileId>,
    entries: Option<ReadDir>,
}
struct Recovery<'a> {
    repair: bool,
    home: &'a Path,
    context: Option<ContextStageValidator>,
}
impl OriginReaper {
    /// Select an explicit state home without accessing it until `step`.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            context: None,
            registry: OriginRegistry::new(home),
            identity: None,
            entries: None,
        }
    }
    /// Enable immutable context recovery only through a pure runtime validator.
    /// Physical cleanup still proves the exact stage and original writer loss;
    /// validation never supplies native presence or conversation authority.
    pub fn with_context_validator(mut self, validate: ContextStageValidator) -> Self {
        self.context = Some(validate);
        self
    }
    /// Inspect at most eight entries and remove at most sixteen exact files.
    /// Invoke on the existing bounded blocking maintenance worker. There are no
    /// sleeps, callbacks, native IPC, recursive scans or directory removals.
    pub fn step(&mut self) -> Result<OriginReport> {
        self.visit(true)
    }
    /// Observe the next bounded page without repair, locks or process launches.
    /// An empty/missing index is inert; ambiguity counts are page-local, not a
    /// claim to have enumerated every unproved file in arbitrary directories.
    pub fn inspect(&mut self) -> Result<OriginReport> {
        self.visit(false)
    }
    fn visit(&mut self, repair: bool) -> Result<OriginReport> {
        let mut report = OriginReport::default();
        let Some(index) = self.registry.index(false)? else {
            return Ok(report);
        };
        if self.identity.is_some_and(|old| old != index.id) {
            return Err(unavailable());
        }
        self.identity = Some(index.id);
        if self.entries.is_none() {
            self.entries = Some(std::fs::read_dir(&index.path).map_err(|_| unavailable())?);
        }
        for _ in 0..8 {
            let Some(entry) = self.entries.as_mut().and_then(Iterator::next) else {
                self.entries = None;
                break;
            };
            report.visited += 1;
            let result = entry.map_err(|_| unavailable()).and_then(|entry| {
                index.current()?;
                let name = entry.file_name();
                let Some(snapshot) = index.read(&name)? else {
                    return Ok(());
                };
                let value = Record::decode(&index, &name, &snapshot)?;
                recover(
                    &index,
                    &name,
                    &snapshot,
                    &value,
                    Recovery {
                        repair,
                        home: &self.registry.home,
                        context: self.context,
                    },
                    &mut report,
                )
            });
            if result.is_err() {
                report.ambiguous += 1;
                report.preserved += 1;
            }
        }
        debug_assert!(report.visited <= 8 && report.removed <= 16);
        Ok(report)
    }
}
fn parent(value: &Record) -> Result<Directory> {
    for path in
        std::iter::once(&value.intent.parent.path).chain(value.intent.parent.alternate.iter())
    {
        if let Ok(directory) = Directory::shared(&record::path(path))
            && directory.id == value.intent.parent.id
        {
            return Ok(directory);
        }
    }
    Err(unavailable())
}
fn recover(
    index: &Directory,
    name: &OsStr,
    snapshot: &Snapshot,
    value: &Record,
    policy: Recovery<'_>,
    report: &mut OriginReport,
) -> Result<()> {
    let Recovery {
        repair,
        home,
        context,
    } = policy;
    let context = context.map(|v| (v, home));
    if value.creator.birth_state() != BirthState::Gone {
        report.preserved += 1;
        return Ok(());
    }
    let _index_lock = if repair {
        Some(index.lock_record(name, snapshot)?)
    } else {
        None
    };
    if value.intent.allocation.is_some() {
        return super::allocation::recover(index, name, snapshot, value, repair, report);
    }
    if matches!(
        value.intent.purpose,
        StagePurpose::Signal | StagePurpose::SignalLock
    ) {
        return super::signal::recover(index, name, snapshot, value, home, repair, report);
    }
    if value.intent.proof.is_some()
        && matches!(
            value.intent.purpose,
            StagePurpose::Relay | StagePurpose::Gateway
        )
    {
        // Stage absence is not external allocation or publication proof.
        report.pending_policy += 1;
        report.preserved += 1;
        return Ok(());
    }
    let data = parent(value)?;
    let _guard = if let Some(guard) = &value.intent.guard {
        let name = record::component(&guard.name)?;
        let snapshot = data.read(name)?.ok_or_else(unavailable)?;
        if snapshot.id != guard.id {
            return Err(unavailable());
        }
        if repair {
            Some(data.lock_record(name, &snapshot)?)
        } else {
            None
        }
    } else {
        None
    };
    let stage = record::component(&value.intent.stage)?;
    let current = data.read_bound(stage, value.intent.purpose.limit())?;
    let mut _history_lock = None;
    if current.is_none()
        && value.intent.purpose == StagePurpose::Context
        && let Some(proof) = &value.intent.proof
    {
        let Some((validate, home)) = context else {
            report.pending_policy += 1;
            report.preserved += 1;
            return Ok(());
        };
        context_parent(&data, home)?;
        let target = record::component(&value.intent.target)?;
        let canonical = data
            .read_bound(target, StagePurpose::Context.limit())?
            .ok_or_else(unavailable)?;
        validate(&canonical.bytes, target)?;
        if canonical.bytes.len() as u64 != proof.length
            || <[u8; 32]>::from(Sha256::digest(&canonical.bytes)) != proof.digest
        {
            return Err(unavailable());
        }
        // Stage absence alone never proves its immutable history was published.
        // An exact duplicate winner may have another inode, but must preserve
        // every original byte under its own nonblocking lock through retirement.
        if repair {
            _history_lock = Some(data.lock_record(target, &canonical)?);
            // The writer may have died immediately after rename. Observing
            // canonical bytes does not make its directory entry durable.
            data.sync()?;
        }
    }
    if let Some(current) = current {
        let proof = value.intent.proof.as_ref().ok_or_else(unavailable)?;
        if current.id != proof.id
            || current.bytes.len() as u64 != proof.length
            || current.changed != proof.changed
            || current.modified != proof.modified
            || <[u8; 32]>::from(Sha256::digest(&current.bytes)) != proof.digest
        {
            return Err(unavailable());
        }
        // Context history and paired signal/socket protocols have distinct
        // durability obligations. Generic deletion must never erase their only
        // retry identity, winning genesis or external allocation witness.
        let immutable = value.intent.purpose == StagePurpose::Context;
        let supported = matches!(
            value.intent.purpose,
            StagePurpose::Metadata
                | StagePurpose::Input
                | StagePurpose::Header
                | StagePurpose::Genesis
                | StagePurpose::Certificate
        ) || (immutable && context.is_some());
        if !supported {
            report.pending_policy += 1;
            report.preserved += 1;
            return Ok(());
        }
        let _data_lock = if repair {
            Some(data.lock_record(stage, &current)?)
        } else {
            None
        };
        if value.creator.birth_state() != BirthState::Gone || !index.matches(name, snapshot)? {
            return Err(unavailable());
        }
        if immutable {
            let (validate, home) = context.ok_or_else(unavailable)?;
            context_parent(&data, home)?;
            let target = record::component(&value.intent.target)?;
            validate(&current.bytes, target)?;
            if repair {
                retain_context(&data, stage, target, &current, validate, report)?;
            }
        } else if repair && data.remove_record(stage, &current)? {
            report.removed += 1;
        }
    }
    if repair {
        data.current()?;
        if !Directory::absent(&data.path.join(stage))?
            || value.creator.birth_state() != BirthState::Gone
        {
            return Err(unavailable());
        }
        if index.remove_record(name, snapshot)? {
            report.removed += 1;
        }
    } else {
        report.preserved += 1;
    }
    Ok(())
}

fn context_parent(data: &Directory, home: &Path) -> Result<()> {
    if data.path != home.join("contexts") || Directory::owned(&data.path)?.id != data.id {
        return Err(unavailable());
    }
    Ok(())
}

fn retain_context(
    data: &Directory,
    stage: &OsStr,
    target: &OsStr,
    current: &Snapshot,
    validate: ContextStageValidator,
    report: &mut OriginReport,
) -> Result<()> {
    // Finish only the exact immutable identity already allocated by the original
    // publisher. There is no current-route lookup, repair of a native cursor,
    // fresh UUID, join, heartbeat or application operation in this path.
    match data.publish_new_io(stage, target) {
        Ok(()) => data.sync(),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let canonical = data
                .read_bound(target, StagePurpose::Context.limit())?
                .ok_or_else(unavailable)?;
            validate(&canonical.bytes, target)?;
            if canonical.bytes != current.bytes {
                return Err(unavailable());
            }
            let _canonical = data.lock_record(target, &canonical)?;
            // Canonical history is retained indefinitely. Only a complete exact
            // duplicate stage is discarded under both original inode locks.
            if data.remove_record(stage, current)? {
                report.removed += 1;
            }
            Ok(())
        }
        Err(_) => Err(unavailable()),
    }
}
