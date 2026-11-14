//! Single-writer, whole-record rotation. Logs are disposable, not replay history.
use super::{FILE_BYTES, FILE_COUNT, RECORD_BYTES, Record};
use crate::filesystem as fs;
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub(super) struct Writer {
    directory: PathBuf,
    file: Option<File>,
    _lock: Lock,
    names: [Option<Stamp>; FILE_COUNT],
}
struct Lock(File, bool);
impl Lock {
    fn release(&mut self) -> Result<()> {
        if self.1 {
            fs2::FileExt::unlock(&self.0).map_err(fs::io)?;
            self.1 = false;
        }
        Ok(())
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        // Release the open-description lock before reporting completion. A
        // briefly inherited descriptor must not extend a finished writer or
        // inspection's lifetime until a subprocess reaches close-on-exec.
        let _ = self.release();
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: i64,
    nanos: i64,
}
impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            length: meta.len(),
            modified: meta.mtime(),
            nanos: meta.mtime_nsec(),
        }
    }
}
impl Writer {
    pub fn open(data: PathBuf, project: Uuid) -> Result<Self> {
        fs::private(&data, true)?;
        let directory = data.join("logs");
        fs::directory(&directory)?;
        let lock_path = directory.join("writer.lock");
        check(&lock_path)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .map_err(fs::io)?;
        identity(&lock, &lock_path)?;
        if lock.metadata().map_err(fs::io)?.len() != 0 {
            return Err(fs::error("Diagnostic writer lock must be empty."));
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match fs2::FileExt::try_lock_exclusive(&lock) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        let mut error = Error::new(
                            ErrorCode::Busy,
                            "A diagnostic writer still owns this project log.",
                        );
                        error.retryable = true;
                        return Err(error);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(fs::io(error)),
            }
        }
        let lock = Lock(lock, true);
        identity(&lock.0, &lock_path)?;
        let mut lengths = [None; FILE_COUNT];
        for (index, length) in lengths.iter_mut().enumerate() {
            *length = validate(&path(&directory, index), project, index == 0)?;
        }
        let active = path(&directory, 0);
        let file = open(&active)?;
        if let Some((valid, expected)) = lengths[0] {
            if Stamp::of(&file.metadata().map_err(fs::io)?) != expected {
                return Err(fs::error("Diagnostic file changed after validation."));
            }
            // A killed append may leave only a partial final JSON record.
            // Validate all files first, then discard only that incomplete tail.
            file.set_len(valid).map_err(fs::io)?;
        }
        file.sync_all().map_err(fs::io)?;
        sync(&directory)?;
        let mut names = lengths.map(|entry| entry.map(|(_, stamp)| stamp));
        names[0] = Some(Stamp::of(&file.metadata().map_err(fs::io)?));
        Ok(Self {
            directory,
            file: Some(file),
            _lock: lock,
            names,
        })
    }
    pub fn append(&mut self, record: &Record) -> Result<()> {
        let mut bytes = serde_json::to_vec(record)
            .map_err(|_| fs::error("Cannot encode a diagnostic record."))?;
        bytes.push(b'\n');
        if bytes.len() > RECORD_BYTES {
            return Err(fs::error("Diagnostic record exceeds its fixed bound."));
        }
        fs::private(&self.directory, true)?;
        self.unchanged(0)?;
        identity(&self._lock.0, &self.directory.join("writer.lock"))?;
        let file = self.file.as_ref().expect("active log");
        identity(file, &path(&self.directory, 0))?;
        let length = file.metadata().map_err(fs::io)?.len();
        if length > FILE_BYTES as u64 {
            return Err(fs::error("Active diagnostic file grew beyond its bound."));
        }
        if length + bytes.len() as u64 > FILE_BYTES as u64 {
            self.rotate()?;
        }
        self.file
            .as_mut()
            .expect("active log")
            .write_all(&bytes)
            .map_err(fs::io)?;
        self.names[0] = Some(Stamp::of(
            &self
                .file
                .as_ref()
                .expect("active log")
                .metadata()
                .map_err(fs::io)?,
        ));
        Ok(())
    }
    fn unchanged(&self, index: usize) -> Result<()> {
        let name = path(&self.directory, index);
        let current = if check(&name)? {
            Some(Stamp::of(&std::fs::symlink_metadata(name).map_err(fs::io)?))
        } else {
            None
        };
        if current != self.names[index] {
            return Err(fs::error(
                "Diagnostic name/content metadata changed outside its writer.",
            ));
        }
        Ok(())
    }
    fn rotate(&mut self) -> Result<()> {
        // Refuse foreign replacements before deleting any retained file. The
        // private directory/OS lock coordinate agentchat; arbitrary same-user writers
        // are not a supported concurrent diagnostic-edit protocol.
        for index in 0..FILE_COUNT {
            self.unchanged(index)?;
        }
        self.file
            .as_ref()
            .expect("active log")
            .sync_all()
            .map_err(fs::io)?;
        drop(self.file.take());
        for index in (1..FILE_COUNT).rev() {
            let destination = path(&self.directory, index);
            if index == FILE_COUNT - 1 {
                match std::fs::remove_file(&destination) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(fs::io(e)),
                }
            }
            let source = path(&self.directory, index - 1);
            match std::fs::rename(source, destination) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(fs::io(e)),
            }
        }
        let active = path(&self.directory, 0);
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(&active)
            .map_err(fs::io)?;
        identity(&file, &active)?;
        for index in (1..FILE_COUNT).rev() {
            self.names[index] = self.names[index - 1];
        }
        self.names[0] = Some(Stamp::of(&file.metadata().map_err(fs::io)?));
        self.file = Some(file);
        sync(&self.directory)
    }
    pub fn finish(mut self) -> Result<()> {
        if let Some(file) = &self.file {
            file.sync_all().map_err(fs::io)?;
        }
        sync(&self.directory)?;
        self._lock.release()
    }
}
fn path(directory: &Path, index: usize) -> PathBuf {
    directory.join(if index == 0 {
        "runtime.jsonl".into()
    } else {
        format!("runtime.jsonl.{index}")
    })
}
fn check(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            fs::private(path, false)?;
            if meta.nlink() != 1 {
                return Err(fs::error(
                    "Diagnostic files must not have hard-link aliases.",
                ));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(fs::io(e)),
    }
}
fn identity(file: &File, path: &Path) -> Result<()> {
    check(path)?;
    let named = std::fs::symlink_metadata(path).map_err(fs::io)?;
    let opened = file.metadata().map_err(fs::io)?;
    if named.dev() != opened.dev() || named.ino() != opened.ino() {
        return Err(fs::error(
            "Diagnostic file changed while its writer was active.",
        ));
    }
    Ok(())
}
fn open(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(path)
        .map_err(fs::io)?;
    identity(&file, path)?;
    Ok(file)
}
fn validate(path: &Path, project: Uuid, partial: bool) -> Result<Option<(u64, Stamp)>> {
    if !check(path)? {
        return Ok(None);
    }
    let mut file = File::open(path).map_err(fs::io)?;
    identity(&file, path)?;
    let stamp = Stamp::of(&file.metadata().map_err(fs::io)?);
    let mut bytes = Vec::new();
    (&mut file)
        .take(FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(fs::io)?;
    if bytes.len() > FILE_BYTES {
        return Err(fs::error("Existing diagnostic file exceeds its bound."));
    }
    let mut valid = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if partial && !line.ends_with(b"\n") && line.len() < RECORD_BYTES {
            break;
        }
        let record: Record = serde_json::from_slice(line)
            .map_err(|_| fs::error("Existing diagnostic record is invalid."))?;
        if line.len() > RECORD_BYTES
            || !line.ends_with(b"\n")
            || record.version != 1
            || record.project_id != project
        {
            return Err(fs::error(
                "Existing diagnostic record violates its format/namespace.",
            ));
        }
        valid += line.len();
    }
    identity(&file, path)?;
    if Stamp::of(&file.metadata().map_err(fs::io)?) != stamp {
        return Err(fs::error("Diagnostic file changed during validation."));
    }
    Ok(Some((valid as u64, stamp)))
}
fn sync(directory: &Path) -> Result<()> {
    File::open(directory)
        .map_err(fs::io)?
        .sync_all()
        .map_err(fs::io)
}

pub(super) fn inspect(data: PathBuf, project: Uuid) -> Result<serde_json::Value> {
    use serde_json::json;
    fs::private(&data, true)?;
    let directory = data.join("logs");
    match std::fs::symlink_metadata(&directory) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"state":"absent","retained_files":0,"retained_bytes":0}));
        }
        Err(e) => return Err(fs::io(e)),
    }
    fs::private(&directory, true)?;
    let name = directory.join("writer.lock");
    if !check(&name)? {
        return Ok(json!({"state":"unowned","reason":"Diagnostic writer lock is absent."}));
    }
    let lock = File::open(&name).map_err(fs::io)?;
    identity(&lock, &name)?;
    if lock.metadata().map_err(fs::io)?.len() != 0 {
        return Err(fs::error("Diagnostic writer lock must be empty."));
    }
    match fs2::FileExt::try_lock_shared(&lock) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(json!({"state":"busy"})),
        Err(e) => return Err(fs::io(e)),
    }
    let mut lock = Lock(lock, true);
    identity(&lock.0, &name)?;
    let (mut files, mut bytes, mut tail) = (0, 0, 0);
    for index in 0..FILE_COUNT {
        if let Some((valid, stamp)) = validate(&path(&directory, index), project, index == 0)? {
            files += 1;
            bytes += stamp.length;
            if index == 0 {
                tail = stamp.length - valid;
            }
        }
    }
    lock.release()?;
    Ok(
        json!({"state":"valid","retained_files":files,"retained_bytes":bytes,
        "incomplete_tail_bytes":tail,"file_bytes":FILE_BYTES,"file_count":FILE_COUNT}),
    )
}
