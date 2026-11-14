//! One complete original context stage; never unlink an observed replacement.
use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

type Id = (u64, u64);
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    id: Id,
    size: u64,
    changed: (i64, i64),
    modified: (i64, i64),
}
fn id(m: &Metadata) -> Id {
    (m.dev(), m.ino())
}
fn stamp(m: &Metadata) -> Stamp {
    Stamp {
        id: id(m),
        size: m.len(),
        changed: (m.ctime(), m.ctime_nsec()),
        modified: (m.mtime(), m.mtime_nsec()),
    }
}
fn valid(m: &Metadata, directory: bool) -> bool {
    m.uid() == rustix::process::geteuid().as_raw()
        && m.mode() & 0o777 == if directory { 0o700 } else { 0o600 }
        && if directory {
            m.is_dir()
        } else {
            m.is_file() && m.nlink() == 1
        }
}
fn changed() -> io::Error {
    io::Error::other("The original context publication has changed.")
}

pub(super) struct Staged<'a> {
    parent: File,
    path: PathBuf,
    parent_id: Id,
    name: OsString,
    file: File,
    id: Id,
    expected: &'a [u8],
    complete: Option<Stamp>,
    published: bool,
    origin: agentchat_hosts::staging::FileOrigin,
}
impl<'a> Staged<'a> {
    pub fn prepare(home: &Path, path: &Path, target: &OsStr, bytes: &'a [u8]) -> io::Result<Self> {
        let parent = File::from(rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let metadata = parent.metadata()?;
        if !valid(&metadata, true) || id(&fs::symlink_metadata(path)?) != id(&metadata) {
            return Err(changed());
        }
        let name = OsString::from(format!(".context-{}", Uuid::new_v4()));
        let origin = agentchat_hosts::staging::OriginRegistry::new(home)
            .begin_file(agentchat_hosts::staging::FilePlan {
                parent: path,
                stage: &name,
                target,
                purpose: agentchat_hosts::staging::StagePurpose::Context,
                alternate_parent: None,
                guard: None,
            })
            .map_err(io::Error::other)?;
        let file = File::from(rustix::fs::openat(
            &parent,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?);
        let original = id(&file.metadata()?);
        let mut value = Self {
            parent,
            path: path.into(),
            parent_id: id(&metadata),
            name,
            file,
            id: original,
            expected: bytes,
            complete: None,
            published: false,
            origin,
        };
        value.current()?;
        value.file.write_all(bytes)?;
        value.file.sync_all()?;
        value.complete = Some(value.snapshot()?.ok_or_else(changed)?);
        value
            .origin
            .seal(&value.file, bytes)
            .map_err(io::Error::other)?;
        Ok(value)
    }
    fn current(&self) -> io::Result<()> {
        let path = fs::symlink_metadata(&self.path)?;
        let held = self.parent.metadata()?;
        if !valid(&path, true)
            || !valid(&held, true)
            || id(&path) != self.parent_id
            || id(&held) != self.parent_id
        {
            return Err(changed());
        }
        Ok(())
    }
    fn open(&self) -> io::Result<File> {
        Ok(File::from(rustix::fs::openat(
            &self.parent,
            &self.name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?))
    }
    fn snapshot(&self) -> io::Result<Option<Stamp>> {
        self.current()?;
        let file = match self.open() {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let before = file.metadata()?;
        if !valid(&before, false)
            || id(&before) != self.id
            || before.len() != self.expected.len() as u64
        {
            return Err(changed());
        }
        let mut bytes = Vec::new();
        (&file)
            .take(self.expected.len().saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        let held = self.file.metadata()?;
        let entry = self.open()?.metadata()?;
        self.current()?;
        // Reads may advance atime. Identity, modification stamps, one link and
        // all expected bytes must still describe the same original live FD.
        for value in [&after, &held, &entry] {
            if !valid(value, false) || stamp(value) != stamp(&before) {
                return Err(changed());
            }
        }
        if bytes != self.expected {
            return Err(changed());
        }
        Ok(Some(stamp(&after)))
    }
    pub fn publish(&mut self, target: &OsStr) -> io::Result<()> {
        if self.complete.is_none() || self.snapshot()? != self.complete {
            return Err(changed());
        }
        self.current()?;
        // Both names stay relative to the same held original parent. There is
        // no hard-link window and no pathname-following publication fallback.
        rustix::fs::renameat_with(
            &self.parent,
            &self.name,
            &self.parent,
            target,
            RenameFlags::NOREPLACE,
        )?;
        // A later sync error must retain the published immutable descriptor,
        // even if somebody creates a new foreign entry at the old stage name.
        self.published = true;
        Ok(())
    }
    pub fn sync(&self) -> io::Result<()> {
        self.current()?;
        self.parent.sync_all()?;
        self.current()
    }
    #[cfg(test)]
    pub fn path(&self) -> PathBuf {
        self.path.join(&self.name)
    }
}
impl Drop for Staged<'_> {
    fn drop(&mut self) {
        if self.published || self.complete.is_none() {
            return;
        }
        if self.snapshot().ok().flatten().as_ref() != self.complete.as_ref() {
            return;
        }
        if self.current().is_ok()
            && rustix::fs::unlinkat(&self.parent, &self.name, AtFlags::empty()).is_ok()
        {
            let _ = self.parent.sync_all();
        }
        // Partial/changed/aliased stages are retained for separate authenticated
        // orphan handling, never adopted solely because of a temporary prefix.
    }
}
