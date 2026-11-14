//! The original writer owns index replacement; local drops never adopt changed data.
use super::{
    FilePlan, OriginRegistry,
    record::{self, FileProof, Intent, Parent, Record},
    unavailable,
};
use crate::claude::{
    metadata::{
        directory::{Directory, FileId, Snapshot},
        staged::Staged,
    },
    process::ProcessIdentity,
};
use agentchat_core::Result;
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::Write,
    os::unix::ffi::OsStrExt,
};
use uuid::Uuid;

/// One writer's durable pending file intent. It never grants application,
/// conversation, native root, presence, admission or wake authority.
pub struct FileOrigin {
    pub(super) index: Directory,
    pub(super) data: Directory,
    pub(super) value: Record,
    pub(super) snapshot: Snapshot,
}
pub(super) fn publish(
    index: &Directory,
    name: &OsStr,
    previous: Option<&Snapshot>,
    value: Record,
) -> Result<(Record, Snapshot)> {
    let _lock = previous.map(|p| index.lock_record(name, p)).transpose()?;
    replace(index, name, previous, value)
}
fn replace(
    index: &Directory,
    name: &OsStr,
    previous: Option<&Snapshot>,
    mut value: Record,
) -> Result<(Record, Snapshot)> {
    if let Some(previous) = previous {
        Record::decode(index, name, previous)?;
    }
    // This low-level stage deliberately does not recursively register an origin
    // for the index itself. Its self-bound complete record names this swap too.
    let mut staged = Staged::new(index, ".origin-")?;
    if value.intent.allocation.is_some() {
        staged.keep_on_failure();
    }
    value.file = FileId::of(&staged.file.metadata().map_err(|_| unavailable())?);
    value.temporary = staged.name().as_bytes().into();
    let bytes = value.seal()?;
    staged.file.write_all(&bytes).map_err(|_| unavailable())?;
    #[cfg(test)]
    if let Some(allocation) = &value.intent.allocation
        && allocation.sockets.len() == allocation.expected.len()
    {
        staged.seal(&bytes).map_err(|_| unavailable())?;
        super::allocation::tests::cut(
            "journal-stage",
            &record::path(&value.intent.parent.path).join(record::component(&value.intent.stage)?),
        );
    }
    staged
        .publish(name, &bytes, previous)
        .map_err(|_| unavailable())?;
    let snapshot = index.read(name)?.ok_or_else(unavailable)?;
    if Record::decode(index, name, &snapshot)? != value {
        return Err(unavailable());
    }
    Ok((value, snapshot))
}
pub(super) fn begin(registry: &OriginRegistry, plan: FilePlan<'_>) -> Result<FileOrigin> {
    record::component(plan.stage.as_bytes())?;
    record::component(plan.target.as_bytes())?;
    if plan.stage == plan.target {
        return Err(unavailable());
    }
    let parent_path = record::absolute(plan.parent)?;
    let alternate = plan.alternate_parent.map(record::absolute).transpose()?;
    let data = Directory::shared(plan.parent)?;
    if !Directory::absent(&plan.parent.join(plan.stage))? {
        return Err(unavailable());
    }
    let index = registry.index(true)?.ok_or_else(unavailable)?;
    let guard = plan
        .guard
        .map(|name| {
            record::component(name.as_bytes())?;
            if name == plan.stage || name == plan.target {
                return Err(unavailable());
            }
            let snapshot = data.read(name)?.ok_or_else(unavailable)?;
            Ok(record::Guard {
                name: name.as_bytes().into(),
                id: snapshot.id,
            })
        })
        .transpose()?;
    let name = OsString::from(format!("{}.json", Uuid::new_v4()));
    let value = Record {
        version: 1,
        index: index.id,
        file: FileId(0, 0),
        name: name.as_bytes().into(),
        temporary: Vec::new(),
        creator: ProcessIdentity::capture(std::process::id())?,
        intent: Intent {
            parent: Parent {
                path: parent_path,
                id: data.id,
                alternate,
            },
            stage: plan.stage.as_bytes().into(),
            target: plan.target.as_bytes().into(),
            purpose: plan.purpose,
            proof: None,
            guard,
            related: None,
            allocation: None,
        },
        checksum: [0; 32],
        retiring: false,
    };
    let (value, snapshot) = publish(&index, &name, None, value)?;
    data.current()?;
    Ok(FileOrigin {
        index,
        data,
        value,
        snapshot,
    })
}
/// Advance only this original dead writer's compatible self-bound frame. A
/// complete interrupted index swap may finish the original canonical key.
pub(super) fn checkpoint(
    index: &Directory,
    name: &OsStr,
    snapshot: &Snapshot,
    value: &Record,
) -> Result<usize> {
    let canonical = record::component(&value.name)?;
    let previous = if canonical == name {
        Some(snapshot.clone())
    } else {
        index.read(canonical)?
    };
    let _other = if canonical != name {
        previous
            .as_ref()
            .map(|p| index.lock_record(canonical, p))
            .transpose()?
    } else {
        None
    };
    if let Some(old) = &previous {
        let old = Record::decode(index, canonical, old)?;
        if old.creator != value.creator
            || old
                .intent
                .proof
                .as_ref()
                .is_some_and(|p| Some(p) != value.intent.proof.as_ref())
            || old
                .intent
                .related
                .as_ref()
                .is_some_and(|p| Some(p) != value.intent.related.as_ref())
        {
            return Err(unavailable());
        }
        let mut plan = old.intent;
        if let Some(old) = &plan.allocation {
            if value
                .intent
                .allocation
                .as_ref()
                .is_none_or(|new| !new.extends(old))
            {
                return Err(unavailable());
            }
            plan.allocation = value.intent.allocation.clone();
        }
        plan.proof = value.intent.proof.clone();
        plan.related = value.intent.related.clone();
        if plan != value.intent {
            return Err(unavailable());
        }
    }
    if !index.matches(name, snapshot)? {
        return Err(unavailable());
    }
    let mut updated = value.clone();
    updated.retiring = true;
    replace(index, canonical, previous.as_ref(), updated)?;
    if canonical != name && index.remove_record(name, snapshot)? {
        Ok(1)
    } else {
        Ok(0)
    }
}
impl FileOrigin {
    fn update(&mut self, value: Record) -> Result<()> {
        let name = record::component(&value.name)?.to_owned();
        let (value, snapshot) = publish(&self.index, &name, Some(&self.snapshot), value)?;
        self.value = value;
        self.snapshot = snapshot;
        self.current()
    }
    /// Pair only complete original stages from this same physical writer and
    /// index. Partial linkage remains ambiguous if the writer dies here.
    pub(crate) fn pair(&mut self, other: &mut Self) -> Result<()> {
        self.current()?;
        other.current()?;
        if self.index.id != other.index.id
            || self.value.creator != other.value.creator
            || !matches!(
                (self.value.intent.purpose, other.value.intent.purpose),
                (super::StagePurpose::Signal, super::StagePurpose::SignalLock)
                    | (super::StagePurpose::SignalLock, super::StagePurpose::Signal)
            )
        {
            return Err(unavailable());
        }
        let mut first = self.value.clone();
        let mut second = other.value.clone();
        first.intent.related = Some(record::Related::capture(&second)?);
        second.intent.related = Some(record::Related::capture(&first)?);
        self.update(first)?;
        other.update(second)
    }
    fn current(&self) -> Result<()> {
        if !self.value.creator.is_current()
            || !self
                .index
                .matches(record::component(&self.value.name)?, &self.snapshot)?
        {
            return Err(unavailable());
        }
        self.data.current()
    }
    /// Synchronize and record the exact original created file and complete bytes.
    /// The caller retains this original FD until publication/cancellation. A
    /// nonblocking inode lock also preserves a listener-like inherited writer.
    pub fn seal(&mut self, file: &File, expected: &[u8]) -> Result<()> {
        self.current()?;
        if expected.len() > self.value.intent.purpose.limit() {
            return Err(unavailable());
        }
        fs2::FileExt::try_lock_exclusive(file).map_err(|_| unavailable())?;
        file.sync_all().map_err(|_| unavailable())?;
        let before = file.metadata().map_err(|_| unavailable())?;
        let snapshot = self
            .data
            .read_bound(
                record::component(&self.value.intent.stage)?,
                expected.len().max(1),
            )?
            .ok_or_else(unavailable)?;
        let after = file.metadata().map_err(|_| unavailable())?;
        let proof = FileProof::capture(&after, expected);
        if snapshot.id != proof.id
            || snapshot.bytes != expected
            || after.len() != expected.len() as u64
            || FileProof::capture(&before, expected) != proof
        {
            return Err(unavailable());
        }
        if self
            .value
            .intent
            .proof
            .as_ref()
            .is_some_and(|old| old != &proof)
        {
            return Err(unavailable());
        }
        let mut value = self.value.clone();
        value.intent.proof = Some(proof);
        self.update(value)
    }
    /// Retire only this exact origin record after its original stage is absent.
    /// This never follows or removes a canonical descriptor or changed stage.
    pub fn finish(&mut self) -> Result<()> {
        self.current()?;
        if !Directory::absent(
            &self
                .data
                .path
                .join(record::component(&self.value.intent.stage)?),
        )? {
            return Err(unavailable());
        }
        let name = record::component(&self.value.name)?;
        let _lock = self.index.lock_record(name, &self.snapshot)?;
        if self.value.intent.purpose == super::StagePurpose::SignalLock
            && let Some(related) = &self.value.intent.related
        {
            // Keep the lock's complete companion proof until the signal origin
            // finishes first. A pending winning genesis must remain retryable.
            if !Directory::absent(&self.index.path.join(record::component(&related.name)?))? {
                return Err(unavailable());
            }
            let parent = Directory::shared(&record::path(&related.parent.path))?;
            if parent.id != related.parent.id
                || !Directory::absent(&parent.path.join(record::component(&related.stage)?))?
            {
                return Err(unavailable());
            }
        }
        if matches!(
            self.value.intent.purpose,
            super::StagePurpose::Context
                | super::StagePurpose::Signal
                | super::StagePurpose::SignalLock
        ) && self.value.intent.proof.is_some()
        {
            // Absence may follow rename before the caller's parent sync. The
            // immutable name change must be durable before its proof retires,
            // including on an error-path drop. Sync refusal keeps the witness.
            self.data.sync()?;
        }
        self.index.remove_record(name, &self.snapshot)?;
        Ok(())
    }
}
impl Drop for FileOrigin {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
