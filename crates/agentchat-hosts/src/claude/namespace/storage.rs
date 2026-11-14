//! Bounded, private, crash-atomic metadata; no lock crosses an asynchronous wait.
use super::{OwnerNamespace, genesis, private, state::Header, unavailable};
use crate::claude::metadata::{directory::Directory, staged::Staged};
use crate::lock::ScopedLock;
use agentchat_core::{Error, ErrorCode, Result};
use fs2::FileExt;
use std::{
    fs::{File, Metadata, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::{Duration, Instant},
};

const LIMIT: usize = 262_144;
pub(super) fn inspect<T>(
    ns: &OwnerNamespace,
    read: impl FnOnce(&Header) -> Result<T>,
) -> Result<T> {
    crate::claude::metadata::private_directory(&ns.directory)?;
    if !ns.owner.is_continuing() {
        return Err(unavailable());
    }
    let path = ns.directory.join("owner-state.lock");
    let deadline = Instant::now() + Duration::from_secs(2);
    let lock = loop {
        let file = genesis::open(ns, false, false)?;
        match FileExt::try_lock_shared(&file) {
            Ok(()) => break ScopedLock::acquired(file),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(unavailable()),
        }
        if Instant::now() >= deadline {
            return Err(busy());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let lock_id = identity(&lock.metadata().map_err(|_| unavailable())?);
    if let Some((record, sealed)) = genesis::record(ns, &lock)?
        && (!sealed || !genesis::marked(ns, &record)?)
    {
        return Err(busy());
    }
    let state_path = ns.directory.join("owner-state.json");
    let before = identity(&metadata(&state_path, LIMIT as u64)?);
    let header: Header = read_record(&state_path, LIMIT)?;
    header.validate(ns, lock_id)?;
    if !ns.owner.is_continuing()
        || identity(&metadata(&path, 65_536)?) != lock_id
        || identity(&metadata(&state_path, LIMIT as u64)?) != before
    {
        return Err(unavailable());
    }
    read(&header)
}
pub(super) fn identity(m: &Metadata) -> (u64, u64) {
    (m.dev(), m.ino())
}
fn metadata(path: &Path, bound: u64) -> Result<Metadata> {
    let m = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
    if !m.is_file()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
        || m.len() > bound
    {
        return Err(unavailable());
    }
    Ok(m)
}
pub(super) fn open(path: &Path, bound: u64, write: bool) -> Result<File> {
    let named = metadata(path, bound)?;
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|_| unavailable())?;
    if identity(&file.metadata().map_err(|_| unavailable())?) != identity(&named)
        || identity(&metadata(path, bound)?) != identity(&named)
    {
        return Err(unavailable());
    }
    Ok(file)
}
pub(super) fn busy() -> Error {
    let mut e = Error::new(
        ErrorCode::Busy,
        "The native owner cursor is busy or incompletely initialized.",
    );
    e.retryable = true;
    e
}
pub(super) fn access<T>(
    namespace: &OwnerNamespace,
    change: impl FnOnce(&mut Header) -> Result<T>,
) -> Result<T> {
    let directory = &namespace.directory;
    private(directory)?;
    let parent = Directory::owned(directory)?;
    if !namespace.owner.is_continuing() {
        return Err(unavailable());
    }
    let state_path = directory.join("owner-state.json");
    let lock_path = directory.join("owner-state.lock");
    let deadline = Instant::now() + Duration::from_secs(2);
    let lock = loop {
        let file = genesis::open(namespace, true, true)?;
        match file.try_lock_exclusive() {
            Ok(()) => break ScopedLock::acquired(file),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(unavailable()),
        }
        if Instant::now() >= deadline {
            return Err(busy());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let lock_identity = identity(&lock.metadata().map_err(|_| unavailable())?);
    let record = genesis::record(namespace, &lock)?;
    let mut previous = match std::fs::symlink_metadata(&state_path) {
        Ok(_) => Some(identity(&metadata(&state_path, LIMIT as u64)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(unavailable()),
    };
    let mut header = if previous.is_some() {
        read_record::<Header>(&state_path, LIMIT)?
    } else {
        let Some((record, sealed)) = &record else {
            return Err(busy());
        };
        if *sealed || genesis::marked(namespace, record)? {
            return Err(busy());
        }
        let header = Header::new(namespace, lock_identity);
        // Header zero is durable before the immutable initialized marker. A
        // marker can therefore never attest to a yet-unpublished first header.
        persist(&parent, namespace, &header, None)?;
        previous = Some(identity(&metadata(&state_path, LIMIT as u64)?));
        header
    };
    header.validate(namespace, lock_identity)?;
    if let Some((record, sealed)) = &record {
        let marked = genesis::marked(namespace, record)?;
        if *sealed {
            if !marked {
                return Err(unavailable());
            }
        } else {
            // No callback may advance any floor before both durable witnesses.
            // Only this pristine prefix can finish after creator interruption.
            if !header.pristine() {
                return Err(unavailable());
            }
            if !marked {
                genesis::mark(namespace, record)?;
            }
            genesis::seal(&lock)?;
        }
    }
    let before = serialize(&header)?;
    let result = change(&mut header)?;
    header.validate(namespace, lock_identity)?;
    let after = serialize(&header)?;
    private(directory)?;
    parent.current()?;
    if !namespace.owner.is_continuing() || identity(&metadata(&lock_path, 65_536)?) != lock_identity
    {
        return Err(unavailable());
    }
    if previous.is_some() && Some(identity(&metadata(&state_path, LIMIT as u64)?)) != previous {
        return Err(unavailable());
    }
    if before != after {
        persist(&parent, namespace, &header, previous)?;
    }
    // Closing the exact open file releases the kernel lock, including on errors.
    drop(lock);
    Ok(result)
}
pub(super) fn persist(
    parent: &Directory,
    namespace: &OwnerNamespace,
    header: &Header,
    previous: Option<(u64, u64)>,
) -> Result<()> {
    let path = namespace.directory.join("owner-state.json");
    if parent.path != namespace.directory {
        return Err(unavailable());
    }
    let prior = parent.read_bound("owner-state.json".as_ref(), LIMIT)?;
    if prior.as_ref().map(|s| (s.id.0, s.id.1)) != previous {
        return Err(unavailable());
    }
    let data = serialize(header)?;
    let alternate = namespace
        .directory
        .parent()
        .ok_or_else(unavailable)?
        .join(format!(
            ".retired-{}",
            namespace
                .directory
                .file_name()
                .ok_or_else(unavailable)?
                .to_string_lossy()
        ));
    let mut stage = Staged::tracked(
        parent,
        ".owner-state-",
        &namespace.home,
        "owner-state.json".as_ref(),
        crate::staging::StagePurpose::Header,
        Some(&alternate),
        Some("owner-state.lock".as_ref()),
    )?;
    stage.file.write_all(&data).map_err(|_| unavailable())?;
    stage
        .publish(
            path.file_name().ok_or_else(unavailable)?,
            &data,
            prior.as_ref(),
        )
        .map_err(|_| unavailable())
}

fn serialize(header: &Header) -> Result<Vec<u8>> {
    struct Limited(Vec<u8>);
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > LIMIT - self.0.len() {
                return Err(std::io::Error::other("Native cursor bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Limited(Vec::new());
    serde_json::to_writer(&mut writer, header).map_err(|_| unavailable())?;
    Ok(writer.0)
}

pub(super) fn read_record<T: serde::de::DeserializeOwned>(path: &Path, bound: usize) -> Result<T> {
    if bound == 0 || bound > LIMIT {
        return Err(unavailable());
    }
    let parent = Directory::owned(path.parent().ok_or_else(unavailable)?)?;
    let snapshot = parent
        .read_bound(path.file_name().ok_or_else(unavailable)?, bound)?
        .ok_or_else(unavailable)?;
    // Owner headers remain raw versioned records; transport records use the
    // shared inline publication proof. Decode both through the same boundary.
    crate::claude::metadata::decode(
        path,
        &snapshot.bytes,
        (parent.id.0, parent.id.1),
        (snapshot.id.0, snapshot.id.1),
    )
}
