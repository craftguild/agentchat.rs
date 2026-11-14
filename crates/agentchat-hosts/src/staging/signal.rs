//! Paired signal recovery never replays a future counter or creates a watcher.
use super::{
    OriginReport, StagePurpose,
    record::{self, FileProof, Parent, Record},
    unavailable, writer,
};
use crate::claude::{
    metadata::directory::{Directory, Snapshot},
    signal::recovery,
};
use agentchat_core::Result;
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, path::Path};

struct Side<'a> {
    data: Directory,
    stage: &'a OsStr,
    target: &'a OsStr,
    proof: &'a FileProof,
    purpose: StagePurpose,
}
impl<'a> Side<'a> {
    fn open(
        parent: &Parent,
        stage: &'a [u8],
        target: &'a [u8],
        proof: &'a FileProof,
        purpose: StagePurpose,
    ) -> Result<Self> {
        if parent.alternate.is_some() {
            return Err(unavailable());
        }
        let data = Directory::shared(&record::path(&parent.path))?;
        if data.id != parent.id {
            return Err(unavailable());
        }
        Ok(Self {
            data,
            stage: record::component(stage)?,
            target: record::component(target)?,
            proof,
            purpose,
        })
    }
    fn staged(&self) -> Result<Option<Snapshot>> {
        let value = self.data.read_bound(self.stage, self.purpose.limit())?;
        if let Some(value) = &value
            && (value.id != self.proof.id
                || value.bytes.len() as u64 != self.proof.length
                || value.changed != self.proof.changed
                || value.modified != self.proof.modified
                || <[u8; 32]>::from(Sha256::digest(&value.bytes)) != self.proof.digest)
        {
            return Err(unavailable());
        }
        Ok(value)
    }
    fn canonical(&self) -> Result<Option<Snapshot>> {
        self.data.read_bound(
            self.target,
            if self.purpose == StagePurpose::Signal {
                32
            } else {
                65_536
            },
        )
    }
}
fn pending(report: &mut OriginReport) {
    report.pending_policy += 1;
    report.preserved += 1;
}
fn retire(
    index: &Directory,
    name: &OsStr,
    original: &Snapshot,
    repair: bool,
    report: &mut OriginReport,
) -> Result<()> {
    if repair {
        if index.remove_record(name, original)? {
            report.removed += 1;
        }
    } else {
        report.preserved += 1;
    }
    Ok(())
}
pub(super) fn recover(
    index: &Directory,
    name: &OsStr,
    original: &Snapshot,
    value: &Record,
    home: &Path,
    repair: bool,
    report: &mut OriginReport,
) -> Result<()> {
    let Some(proof) = &value.intent.proof else {
        let data = Directory::shared(&record::path(&value.intent.parent.path))?;
        if data.id != value.intent.parent.id {
            return Err(unavailable());
        }
        if Directory::absent(&data.path.join(record::component(&value.intent.stage)?))?
            && value.intent.related.is_none()
        {
            return retire(index, name, original, repair, report);
        }
        pending(report);
        return Ok(());
    };
    let Some(pair) = &value.intent.related else {
        pending(report);
        return Ok(());
    };
    let own = Side::open(
        &value.intent.parent,
        &value.intent.stage,
        &value.intent.target,
        proof,
        value.intent.purpose,
    )?;
    let related = Side::open(
        &pair.parent,
        &pair.stage,
        &pair.target,
        &pair.proof,
        pair.purpose,
    )?;
    let signal_role = value.intent.purpose == StagePurpose::Signal;
    let (signal, lock) = if signal_role {
        (&own, &related)
    } else {
        (&related, &own)
    };
    let anchor = recovery::location(home, &signal.data.path);
    if signal.purpose != StagePurpose::Signal
        || lock.purpose != StagePurpose::SignalLock
        || signal.target != ".agentchat-wakeup"
        || lock.data.path != anchor.parent().ok_or_else(unavailable)?
        || lock.target
            != anchor
                .with_extension("lock")
                .file_name()
                .ok_or_else(unavailable)?
        || Directory::owned(&lock.data.path)?.id != lock.data.id
        || signal.proof.length != 2
        || signal.proof.digest != <[u8; 32]>::from(Sha256::digest(b"0\n"))
    {
        return Err(unavailable());
    }
    let raw_stage = lock.staged()?;
    let raw_canonical = lock.canonical()?;
    let signal_stage = signal.staged()?;
    let signal_canonical = signal.canonical()?;
    if let Some(raw) = &raw_stage {
        recovery::birth(
            &raw.bytes,
            lock.proof.id,
            &signal.data.path,
            signal.proof.id,
            signal.stage,
        )?;
        let _lock = if repair {
            Some(lock.data.lock_record(lock.stage, raw)?)
        } else {
            None
        };
        // A complete original lock still at its exclusive stage was never the
        // canonical winner. Remove the orphan signal first, its lock last.
        if raw_canonical
            .as_ref()
            .is_some_and(|s| s.id == lock.proof.id)
            || signal_canonical
                .as_ref()
                .is_some_and(|s| s.id == signal.proof.id)
        {
            return Err(unavailable());
        }
        if signal_role {
            // The remaining lock witness must retain this full pair after the
            // signal witness retires. A missing/copy/partial mate is ambiguous.
            let mate_name = record::component(&pair.name)?;
            let mate = index.read(mate_name)?.ok_or_else(unavailable)?;
            let mate = Record::decode(index, mate_name, &mate)?;
            if mate.creator != value.creator
                || mate.intent.proof.as_ref() != Some(lock.proof)
                || mate.intent.parent != pair.parent
                || mate.intent.stage != pair.stage
                || mate.intent.target != pair.target
                || mate.intent.purpose != pair.purpose
                || mate.intent.related.as_ref() != Some(&record::Related::capture(value)?)
            {
                return Err(unavailable());
            }
            if let Some(stage) = &signal_stage {
                let _held = if repair {
                    Some(signal.data.lock_record(signal.stage, stage)?)
                } else {
                    None
                };
                if repair && signal.data.remove_record(signal.stage, stage)? {
                    report.removed += 1;
                    #[cfg(test)]
                    crate::claude::signal::tests::origins::cut(
                        "signal-removed",
                        &signal.data.path,
                        &lock.data.path.join(lock.target),
                    );
                }
            }
            if repair {
                signal.data.sync()?;
            }
            return retire(index, name, original, repair, report);
        }
        if signal_stage.is_some()
            || !Directory::absent(&index.path.join(record::component(&pair.name)?))?
        {
            pending(report);
            return Ok(());
        }
        if !repair {
            report.preserved += 1;
            return Ok(());
        }
        if !value.retiring || name != record::component(&value.name)? {
            // A durable cancellation checkpoint makes the unlink-to-witness
            // cut retryable. Swap frames finish the same original index key.
            report.removed += writer::checkpoint(index, name, original, value)?;
            report.preserved += 1;
            return Ok(());
        }
        if lock.data.remove_record(lock.stage, raw)? {
            report.removed += 1;
            #[cfg(test)]
            crate::claude::signal::tests::origins::cut(
                "lock-removed",
                &signal.data.path,
                &lock.data.path.join(lock.target),
            );
        }
        return retire(index, name, original, true, report);
    }
    if !signal_role
        && value.retiring
        && raw_canonical.as_ref().is_none_or(|s| s.id != lock.proof.id)
        && signal_stage.is_none()
        && signal_canonical
            .as_ref()
            .is_none_or(|s| s.id != signal.proof.id)
        && Directory::absent(&index.path.join(record::component(&pair.name)?))?
    {
        if repair {
            signal.data.sync()?;
            lock.data.sync()?;
        }
        return retire(index, name, original, repair, report);
    }
    let raw = raw_canonical.as_ref().ok_or_else(unavailable)?;
    if raw.id != lock.proof.id || raw.bytes.len() as u64 != lock.proof.length {
        return Err(unavailable());
    }
    let sealed = recovery::birth(
        &raw.bytes,
        lock.proof.id,
        &signal.data.path,
        signal.proof.id,
        signal.stage,
    )?;
    let mut normalized = raw.bytes.clone();
    normalized[0] = b'0';
    if <[u8; 32]>::from(Sha256::digest(&normalized)) != lock.proof.digest {
        return Err(unavailable());
    }
    let _lock = if repair {
        Some(lock.data.lock_record(lock.target, raw)?)
    } else {
        None
    };
    if signal_stage.is_some() {
        if sealed || signal_canonical.is_some() || !Directory::absent(&anchor)? {
            return Err(unavailable());
        }
        // Keep the winning original zero stage for ordinary Birth::signal;
        // maintenance does not create a watched file or wake a model.
        pending(report);
        return Ok(());
    }
    let canonical = signal_canonical.as_ref().ok_or_else(unavailable)?;
    if canonical.id != signal.proof.id {
        return Err(unavailable());
    }
    let _signal = if repair {
        Some(signal.data.lock_record(signal.target, canonical)?)
    } else {
        None
    };
    recovery::history(&anchor, &signal.data.path, canonical, sealed)?;
    if repair {
        signal.data.sync()?;
        lock.data.sync()?;
    }
    retire(index, name, original, repair, report)
}
