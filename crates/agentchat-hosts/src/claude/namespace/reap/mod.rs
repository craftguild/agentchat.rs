//! Bounded retained traversal and durable exact-owner quarantine finalization.
mod cleanup;
mod proof;
use super::snapshot as files;
use super::{OwnerNamespace, unavailable};
use crate::claude::{NativeLifecycle, metadata};
use agentchat_core::{ErrorCode, Result};
use proof::{Certificate, Phase};
use std::{
    fs::ReadDir,
    path::{Component, PathBuf},
};

const VISITS: usize = 8;
/// Counters for one finite maintenance pass; none is host presence or ACK proof.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReapReport {
    /// Candidate/child entries visited, at most eight per pass.
    pub visited: usize,
    /// Successfully removed private metadata files, at most forty per pass.
    pub removed: usize,
    /// Original empty socket/quarantine directories removed, at most one per pass.
    pub directories_removed: usize,
    /// Busy, changed, foreign, live or uncertain candidates preserved.
    pub preserved: usize,
    /// Another collector holds the fixed global maintenance lock.
    pub busy: bool,
}
struct Work {
    namespace: OwnerNamespace,
    certificate: Certificate,
    entries: Option<ReadDir>,
}
/// Stateful private metadata collector. Retaining it between bounded `step`
/// calls advances directory traversal beyond live/foreign front entries.
/// A new instance recovers durable quarantine/certificate deletion prefixes;
/// it never initializes a project, joins, renews presence or repairs live state.
pub struct OwnerReaper {
    origins: crate::staging::OriginReaper,
    launches: crate::launch::LaunchReaper,
    turn: usize,
    home: PathBuf,
    root: PathBuf,
    identity: Option<(u64, u64)>,
    entries: Option<ReadDir>,
    active: Option<Work>,
}
impl OwnerReaper {
    /// Prepare lazy maintenance without creating state or inspecting a cwd.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            origins: crate::staging::OriginReaper::new(home.clone()),
            launches: crate::launch::LaunchReaper::new(home.clone()),
            turn: 0,
            root: home.join("hosts/claude/owners"),
            home,
            identity: None,
            entries: None,
            active: None,
        }
    }
    /// Compose pure immutable runtime-descriptor validation without adding a
    /// worker, timer, history scan or application/native callback under locks.
    pub fn with_context_validator(
        mut self,
        validate: crate::staging::ContextStageValidator,
    ) -> Self {
        self.origins = self.origins.with_context_validator(validate);
        self
    }
    /// Visit at most eight entries and perform only finite synchronous private
    /// file operations and a nonblocking local socket probe. Invoke on a bounded
    /// blocking worker; no lock may cross native/application callbacks, blocking
    /// network I/O or asynchronous work. Missing roots are inert.
    pub fn step(&mut self) -> Result<ReapReport> {
        // Origins, launch proofs and native owners share the same finite budget
        // and blocking worker. Empty pages spend no visits; no additional timer
        // or asynchronous cleanup scheduling is introduced for launch metadata.
        for _ in 0..3 {
            let turn = self.turn;
            self.turn = (turn + 1) % 3;
            let report = match turn {
                0 => {
                    let p = self.origins.step()?;
                    ReapReport {
                        visited: p.visited,
                        removed: p.removed,
                        directories_removed: p.directories_removed,
                        preserved: p.preserved,
                        ..Default::default()
                    }
                }
                1 => {
                    let p = self.launches.step()?;
                    ReapReport {
                        visited: p.visited,
                        removed: p.removed,
                        preserved: p.preserved,
                        ..Default::default()
                    }
                }
                _ => return self.step_observed(&mut |_| {}),
            };
            if report.visited != 0 {
                return Ok(report);
            }
        }
        Ok(ReapReport::default())
    }
    fn step_observed(&mut self, hook: &mut dyn FnMut(&str)) -> Result<ReapReport> {
        let mut report = ReapReport::default();
        if !self.home.is_absolute() || self.home.components().any(|c| c == Component::ParentDir) {
            return Err(unavailable());
        }
        if !files::exists(&self.root)? {
            return Ok(report);
        }
        let mut path = self.home.clone();
        metadata::private_directory(&path)?;
        for part in ["hosts", "claude", "owners"] {
            path.push(part);
            metadata::private_directory(&path)?;
        }
        let identity = files::directory(&self.root)?;
        if self.identity.is_some_and(|prior| prior != identity) {
            return Err(unavailable());
        }
        self.identity = Some(identity);
        let _global = match files::locked(&self.root.join(".collection.lock"), true) {
            Ok(lock) => lock,
            Err(error) if error.code == ErrorCode::Busy => {
                report.busy = true;
                return Ok(report);
            }
            Err(error) => return Err(error),
        };
        for _ in 0..VISITS {
            if let Some(mut work) = self.active.take() {
                report.visited += 1;
                match self.advance(&mut work, &mut report, hook) {
                    Ok(true) => self.active = Some(work),
                    Ok(false) => {}
                    Err(_) => report.preserved += 1,
                }
                // Finalization can remove four witnesses and the certificate;
                // return after one directory, keeping the per-pass bound simple.
                if report.directories_removed != 0 {
                    break;
                }
                continue;
            }
            if self.entries.is_none() {
                self.entries = Some(std::fs::read_dir(&self.root).map_err(|_| unavailable())?);
            }
            let Some(entry) = self.entries.as_mut().and_then(Iterator::next) else {
                self.entries = None;
                break;
            };
            report.visited += 1;
            let entry = entry.map_err(|_| unavailable())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                report.preserved += 1;
                continue;
            };
            let key = name
                .strip_prefix(".retired-")
                .or_else(|| {
                    name.strip_prefix(".gc-")
                        .and_then(|s| s.strip_suffix(".json"))
                })
                .unwrap_or(name);
            if key.len() != 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                continue;
            }
            let certificate = self.root.join(format!(".gc-{key}.json"));
            let admitted = if files::exists(&certificate)? {
                proof::load(&self.home, &certificate)
            } else {
                proof::admit(&self.home, &entry.path(), hook)
            };
            match admitted {
                Ok((namespace, certificate)) => {
                    self.active = Some(Work {
                        namespace,
                        certificate,
                        entries: None,
                    })
                }
                Err(_) => report.preserved += 1,
            }
        }
        debug_assert!(
            report.visited <= VISITS && report.removed <= 40 && report.directories_removed <= 1
        );
        Ok(report)
    }
    fn advance(
        &self,
        work: &mut Work,
        report: &mut ReapReport,
        hook: &mut dyn FnMut(&str),
    ) -> Result<bool> {
        let path = proof::certificate_path(&self.root, &work.namespace)?;
        let (_, actual) = proof::load(&self.home, &path)?;
        if actual != work.certificate {
            return Err(unavailable());
        }
        if actual.phase == Phase::Finalizing || !files::exists(&work.namespace.directory)? {
            cleanup::finalize(&self.home, &work.namespace, &actual, report, hook)?;
            return Ok(false);
        }
        let (lock, _gateway) = proof::locks(&work.namespace)?;
        let header = proof::verify(&work.namespace, &actual, &lock)?;
        cut(hook, "validated");
        if let Some(current) = &header.current
            && let NativeLifecycle::Project(r) = &current.lifecycle
        {
            match cleanup::notices(&self.home, &work.namespace, r, actual.directory) {
                Ok(count) => report.removed += count,
                Err(_) => report.preserved += 1,
            }
        }
        if work.entries.is_none() {
            work.entries =
                Some(std::fs::read_dir(&work.namespace.directory).map_err(|_| unavailable())?);
        }
        if let Some(entry) = work.entries.as_mut().and_then(Iterator::next) {
            let entry = entry.map_err(|_| unavailable())?;
            if files::exists(&entry.path())? {
                match cleanup::one(
                    &self.home,
                    &work.namespace,
                    &header,
                    &actual,
                    &entry.path(),
                    report,
                ) {
                    Ok(count) => {
                        report.removed += count;
                        if count != 0 {
                            cut(hook, "projection");
                        }
                    }
                    Err(_) => report.preserved += 1,
                }
            }
            return Ok(true);
        }
        if cleanup::ready(&work.namespace, &actual)? {
            cleanup::finalize(&self.home, &work.namespace, &actual, report, hook)?;
        } else {
            report.preserved += 1;
        }
        Ok(false)
    }
}
fn cut(hook: &mut dyn FnMut(&str), point: &str) {
    hook(point);
}

#[cfg(test)]
#[path = "../../../../tests/unit/native_reaper.rs"]
mod tests;
