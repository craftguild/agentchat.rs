//! Paired physical intent precedes either first-creator allocation.
use super::*;
use crate::{
    claude::metadata::{
        directory::{Directory, FileId},
        staged::{Retained, Staged},
    },
    staging::StagePurpose,
};

pub(super) fn publish(
    home: &Path,
    path: &Path,
    anchor_path: &Path,
    cwd: &Path,
) -> Result<(Option<Retained>, Option<Retained>)> {
    let signal = cwd.join(".agentchat-wakeup");
    let shared = Directory::shared(cwd)?;
    let mut stage = None;
    let candidate = (|| {
        if exists(&signal)? {
            let file = signal_file(&signal)?;
            let anchor: Anchor = metadata::read(anchor_path)?;
            let id = identity(&checked(&signal, &file)?);
            if anchor.version != 1
                || anchor.cwd != cwd
                || FileId(anchor.dev, anchor.ino) != id
                || anchor.floor > MAX
                || anchor.pending.is_some()
            {
                return Err(required());
            }
            Ok((id, None))
        } else {
            if exists(anchor_path)? {
                return Err(required());
            }
            let mut file = Staged::tracked(
                &shared,
                ".agentchat-wakeup-stage-",
                home,
                ".agentchat-wakeup".as_ref(),
                StagePurpose::Signal,
                None,
                None,
            )?;
            file.file.write_all(b"0\n").map_err(|_| required())?;
            file.seal(b"0\n").map_err(|_| required())?;
            shared.sync()?;
            let id = FileId::of(&file.file.metadata().map_err(|_| required())?);
            let name = file.name().to_str().ok_or_else(required)?.to_owned();
            stage = Some(file);
            Ok((id, Some(name)))
        }
    })();
    let (signal_identity, staging) = match candidate {
        Ok(value) => value,
        Err(_) if exists(path)? => return Ok((None, None)),
        Err(error) => return Err(error),
    };
    let held = Directory::owned(path.parent().ok_or_else(required)?)?;
    let mut lock = Staged::tracked(
        &held,
        ".signal-lock-",
        home,
        path.file_name().ok_or_else(required)?,
        if stage.is_some() {
            StagePurpose::SignalLock
        } else {
            StagePurpose::Genesis
        },
        None,
        None,
    )?;
    let record = Record {
        version: 1,
        cwd: cwd.into(),
        lock_identity: identity(&lock.file.metadata().map_err(|_| required())?),
        signal_identity,
        staging,
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| required())?;
    if bytes.len() > LIMIT as usize - 2 {
        return Err(required());
    }
    let data = [b"0\n".as_slice(), &bytes].concat();
    lock.file.write_all(&data).map_err(|_| required())?;
    lock.seal(&data).map_err(|_| required())?;
    if let Some(signal) = stage.as_mut() {
        lock.pair_with(signal)?;
    }
    #[cfg(test)]
    super::super::tests::origins::cut("paired", cwd, path);
    let own_id = record.lock_identity;
    match lock.publish_retained(path.file_name().ok_or_else(required)?, &data) {
        Ok(lock) => {
            #[cfg(test)]
            super::super::tests::origins::cut("lock-published", cwd, path);
            if identity(&private_file(path)?) != own_id {
                return Err(required());
            }
            let signal = stage.map(Staged::retain_pending).transpose()?;
            Ok((signal, Some(lock)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok((None, None)),
        Err(_) => {
            // A failed parent sync can follow the private lock's visible rename.
            // Its pending record may already name this exact signal stage; do
            // not delete the paired data merely because publication is unknown.
            if let Some(signal) = stage.as_mut() {
                signal.keep_on_failure();
            }
            Err(required())
        }
    }
}
