//! Declare the staged inode durably before publishing its watched name.
use super::*;
mod publication;
use publication::publish;
use std::os::unix::fs::FileExt;

const LIMIT: u64 = 65_536;
pub(super) fn validate_recovery(
    raw: &[u8],
    lock: FileId,
    cwd: &Path,
    signal: FileId,
    stage: &std::ffi::OsStr,
) -> Result<bool> {
    let sealed = match raw.get(..2) {
        Some(b"0\n") => false,
        Some(b"1\n") => true,
        _ => return Err(required()),
    };
    let value: Record = serde_json::from_slice(&raw[2..]).map_err(|_| required())?;
    if value.version != 1
        || value.lock_identity != lock
        || value.cwd != cwd
        || value.signal_identity != signal
        || value.staging.as_deref().map(std::ffi::OsStr::new) != Some(stage)
    {
        return Err(required());
    }
    Ok(sealed)
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    cwd: PathBuf,
    lock_identity: FileId,
    signal_identity: FileId,
    staging: Option<String>,
}
pub(super) struct Birth {
    signal_origin: Option<crate::staging::FileOrigin>,
    lock_origin: Option<crate::staging::FileOrigin>,
    signal_file: Option<File>,
    publisher: metadata::Publisher,
    file: ScopedLock,
    path: PathBuf,
    record: Record,
    sealed: bool,
}
fn identity(m: &std::fs::Metadata) -> FileId {
    FileId::of(m)
}
fn private_file(path: &Path) -> Result<std::fs::Metadata> {
    let m = std::fs::symlink_metadata(path).map_err(|_| required())?;
    if !m.is_file()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
        || m.len() > LIMIT
    {
        return Err(required());
    }
    Ok(m)
}
fn sync(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| required())
}
impl Birth {
    pub(super) fn open(home: &Path, anchor_path: &Path, cwd: &Path) -> Result<Self> {
        let path = anchor_path.with_extension("lock");
        let (signal, retained_lock) = if !exists(&path)? {
            publish(home, &path, anchor_path, cwd)?
        } else {
            (None, None)
        };
        let (signal_origin, signal_file) = signal.map(|s| (s.origin, s.file)).unzip();
        let (lock_origin, original_lock) = retained_lock.map(|s| (s.origin, s.file)).unzip();
        let before = private_file(&path)?;
        let file = if let Some(file) = original_lock {
            file
        } else {
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(&path)
                .map_err(|_| required())?
        };
        lock(&file)?;
        let mut file = ScopedLock::acquired(file);
        if identity(&before) != identity(&file.metadata().map_err(|_| required())?)
            || identity(&before) != identity(&private_file(&path)?)
        {
            return Err(required());
        }
        // A transferred description shares its writer's end-of-record offset;
        // parsing always starts at byte zero, just as with a fresh open.
        file.seek(SeekFrom::Start(0)).map_err(|_| required())?;
        let mut raw = Vec::new();
        Read::by_ref(&mut *file)
            .take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|_| required())?;
        if raw.len() > LIMIT as usize {
            return Err(required());
        }
        let sealed = match raw.get(..2) {
            Some(b"0\n") => false,
            Some(b"1\n") => true,
            _ => return Err(required()),
        };
        let record: Record = serde_json::from_slice(&raw[2..]).map_err(|_| required())?;
        if record.version != 1
            || record.cwd != cwd
            || record.lock_identity != identity(&before)
            || record.staging.as_ref().is_some_and(|n| {
                !n.starts_with(".agentchat-wakeup-stage-")
                    || Path::new(n).components().count() != 1
                    || n.contains('/')
            })
        {
            return Err(required());
        }
        let birth = Self {
            signal_origin,
            lock_origin,
            signal_file,
            publisher: metadata::Publisher::new(home).guard(path.file_name().ok_or_else(required)?),
            file,
            path,
            record,
            sealed,
        };
        birth.check()?;
        Ok(birth)
    }
    pub(super) fn publisher(&self) -> &metadata::Publisher {
        &self.publisher
    }
    pub(super) fn check(&self) -> Result<()> {
        let named = private_file(&self.path)?;
        if identity(&named) != self.record.lock_identity
            || identity(&self.file.metadata().map_err(|_| required())?) != self.record.lock_identity
        {
            return Err(required());
        }
        let mut raw = vec![0; named.len() as usize];
        self.file
            .read_exact_at(&mut raw, 0)
            .map_err(|_| required())?;
        if raw.get(..2) != Some(if self.sealed { b"1\n" } else { b"0\n" })
            || serde_json::from_slice::<Record>(&raw[2..]).map_err(|_| required())? != self.record
            || identity(&private_file(&self.path)?) != self.record.lock_identity
        {
            return Err(required());
        }
        Ok(())
    }
    pub(super) fn signal(&mut self, path: &Path, anchor_path: &Path) -> Result<File> {
        self.check()?;
        let mut retained = self.signal_file.take();
        if !exists(path)? {
            if self.sealed || exists(anchor_path)? {
                return Err(required());
            }
            let name = self.record.staging.as_ref().ok_or_else(required)?;
            let stage = self.record.cwd.join(name);
            let mut staged = match retained.take() {
                Some(file) => file,
                None => signal_file(&stage)?,
            };
            if identity(&checked(&stage, &staged)?) != self.record.signal_identity
                || bytes(&mut staged)? != b"0\n"
            {
                return Err(required());
            }
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                &stage,
                rustix::fs::CWD,
                path,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|_| required())?;
            sync(&self.record.cwd)?;
            #[cfg(test)]
            super::tests::origins::cut("watched", &self.record.cwd, &self.path);
            retained = Some(staged);
        }
        let file = match retained {
            Some(file) => file,
            None => signal_file(path)?,
        };
        if identity(&checked(path, &file)?) != self.record.signal_identity {
            return Err(required());
        }
        Ok(file)
    }
    pub(super) fn finish(
        &mut self,
        file: &mut File,
        path: &Path,
        anchor_path: &Path,
    ) -> Result<Anchor> {
        self.check()?;
        let identity = identity(&checked(path, file)?);
        let mut anchor: Anchor = if exists(anchor_path)? {
            metadata::read(anchor_path)?
        } else {
            if self.sealed || self.record.staging.is_none() || bytes(file)? != b"0\n" {
                return Err(required());
            }
            let anchor = Anchor {
                version: 2,
                cwd: self.record.cwd.clone(),
                dev: identity.0,
                ino: identity.1,
                floor: 0,
                pending: None,
            };
            self.publisher.create(anchor_path, &anchor)?;
            #[cfg(test)]
            super::tests::origins::cut("anchor", &self.record.cwd, &self.path);
            anchor
        };
        if anchor.cwd != self.record.cwd || FileId(anchor.dev, anchor.ino) != identity {
            return Err(required());
        }
        counter::validate(&anchor)?;
        if !self.sealed {
            if self.record.staging.is_some() {
                if anchor.version != 2
                    || anchor.floor != 0
                    || anchor.pending.is_some()
                    || bytes(file)? != b"0\n"
                {
                    return Err(required());
                }
            } else if anchor.version == 1 {
                anchor = counter::upgrade(anchor, &bytes(file)?)?;
                self.publisher.write(anchor_path, &anchor)?;
            }
            // Directory entries and the complete anchor precede this one-way
            // seal in the original inode. No callback can run before sealing.
            sync(&self.record.cwd)?;
            self.file
                .write_all_at(b"1", 0)
                .and_then(|_| self.file.sync_all())
                .map_err(|_| required())?;
            self.sealed = true;
        }
        if anchor.version != 2 {
            return Err(required());
        }
        self.check()?;
        if let Some(mut origin) = self.signal_origin.take() {
            origin.finish()?;
        }
        if let Some(mut origin) = self.lock_origin.take() {
            origin.finish()?;
        }
        Ok(anchor)
    }
}
