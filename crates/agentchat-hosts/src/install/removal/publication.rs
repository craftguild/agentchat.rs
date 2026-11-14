//! Final preimage checks and held-parent publication preserve foreign profiles.
use super::super::{conflict, fileio};
use super::{Edit, files};
use agentchat_core::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

struct Parent {
    file: File,
    path: PathBuf,
    id: (u64, u64),
}
impl Parent {
    fn open(path: &Path, expected: Option<(u64, u64)>) -> Result<Self> {
        fileio::safe_parents(&path.join("entry"))?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32,
            )
            .open(path)
            .map_err(super::super::io)?;
        let meta = file.metadata().map_err(super::super::io)?;
        let id = (meta.dev(), meta.ino());
        if expected.is_some_and(|old| old != id) {
            return Err(conflict("Host removal parent was replaced."));
        }
        let value = Self {
            file,
            path: path.into(),
            id,
        };
        value.current()?;
        Ok(value)
    }
    fn current(&self) -> Result<()> {
        fileio::safe_parents(&self.path.join("entry"))?;
        let meta = fs::symlink_metadata(&self.path).map_err(super::super::io)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || (meta.dev(), meta.ino()) != self.id {
            return Err(conflict(
                "Host removal parent identity changed before publication.",
            ));
        }
        Ok(())
    }
    fn sync(&self) -> Result<()> {
        self.current()?;
        self.file.sync_all().map_err(super::super::io)
    }
}
pub(super) fn edit(edit: &Edit) -> Result<()> {
    let path = fileio::path(&edit.file)?;
    if files::current(edit)? {
        if path.exists() {
            fileio::durable(&path)?;
        } else if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
            Parent::open(parent, Some(edit.parent))?.sync()?;
        }
        return Ok(());
    }
    let parent = Parent::open(path.parent().expect("file parent"), Some(edit.parent))?;
    let name = path.file_name().expect("file name");
    let Some(after) = &edit.after else {
        files::current(edit)?;
        parent.current()?;
        rustix::fs::unlinkat(&parent.file, name, rustix::fs::AtFlags::empty())
            .map_err(|e| super::super::io(e.into()))?;
        return parent.sync();
    };
    let stage = format!(".agentchat-remove-{}", edit.stage);
    let stage_path = parent.path.join(&stage);
    if fs::symlink_metadata(&stage_path).is_ok() {
        let (identity, mode) = files::identity(&stage_path)?;
        if ![0o600, edit.mode].contains(&mode) || identity.0 != parent.id.0 {
            return Err(conflict(
                "Owned removal staging file has a foreign identity.",
            ));
        }
        // This exact unpredictable name is owned by the durable private plan.
        rustix::fs::unlinkat(&parent.file, stage.as_str(), rustix::fs::AtFlags::empty())
            .map_err(|e| super::super::io(e.into()))?;
    }
    let fd = rustix::fs::openat(
        &parent.file,
        stage.as_str(),
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map_err(|e| super::super::io(e.into()))?;
    let mut staged = File::from(fd);
    let result = (|| {
        staged
            .write_all(after.as_bytes())
            .map_err(super::super::io)?;
        staged
            .set_permissions(fs::Permissions::from_mode(edit.mode))
            .map_err(super::super::io)?;
        staged.sync_all().map_err(super::super::io)?;
        if files::current(edit)? {
            return Err(conflict("Host removal destination changed during staging."));
        }
        parent.current()?;
        // As with preserving installation, the last rename cannot offer an
        // atomic CAS against an unrelated noncooperating native settings editor.
        rustix::fs::renameat(&parent.file, stage.as_str(), &parent.file, name)
            .map_err(|e| super::super::io(e.into()))?;
        parent.sync()
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(&parent.file, stage.as_str(), rustix::fs::AtFlags::empty());
    }
    result
}
pub(super) fn empty_directory(
    path: &Path,
    identity: (u64, u64),
    parent_identity: (u64, u64),
) -> Result<()> {
    let current = files::directory_identity(path)?.0;
    if current != identity {
        return Err(conflict(
            "Exclusive artifact directory changed before removal.",
        ));
    }
    let parent = Parent::open(
        path.parent().expect("directory parent"),
        Some(parent_identity),
    )?;
    if files::directory_identity(path)?.0 != identity {
        return Err(conflict("Exclusive directory changed before unlink."));
    }
    rustix::fs::unlinkat(
        &parent.file,
        path.file_name().expect("directory name"),
        rustix::fs::AtFlags::REMOVEDIR,
    )
    .map_err(|e| super::super::io(e.into()))?;
    parent.sync()
}
