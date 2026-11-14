//! Legacy runtime fences and inactive endpoints complement the new lifetime lease.

use super::{conflict, io, io::Directory};
use crate::{
    daemon::RuntimePaths,
    project::{Project, ProjectManager},
};
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) fn existing_lock(path: &Path) -> Result<Option<File>> {
    let Some(parent) = path.parent().filter(|p| p.is_dir()) else {
        return Ok(None);
    };
    let held = Directory::open(parent)?;
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let meta = file.metadata().map_err(io)?;
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(conflict(
            "Relevant runtime/installer lock has untrusted identity.",
        ));
    }
    fs2::FileExt::try_lock_exclusive(&file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            busy()
        } else {
            io(e)
        }
    })?;
    let current = fs::symlink_metadata(path).map_err(io)?;
    held.current()?;
    if (current.dev(), current.ino()) != (meta.dev(), meta.ino()) {
        return Err(conflict("Relevant lifetime lock was replaced."));
    }
    Ok(Some(file))
}

pub(super) fn runtime_directory(project: &Project) -> Result<PathBuf> {
    runtime_for_data(project.data_dir())
}
pub(super) fn runtime_for_data(data: &Path) -> Result<PathBuf> {
    let paths = RuntimePaths::for_data_directory(data);
    if paths.directory.exists() {
        let held = Directory::open(&paths.directory.canonicalize().map_err(io)?)?;
        let meta = fs::symlink_metadata(&paths.directory).map_err(io)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o077 != 0
        {
            return Err(conflict(
                "Runtime endpoint directory has foreign ownership or type.",
            ));
        }
        inactive(&paths.socket)?;
        held.current()?;
    }
    let parent = paths
        .directory
        .parent()
        .ok_or_else(|| conflict("Missing runtime endpoint parent."))?
        .canonicalize()
        .map_err(io)?;
    Ok(parent.join(paths.directory.file_name().expect("runtime directory name")))
}

pub(super) fn protected_roots(
    manager: &ProjectManager,
    environment: &agentchat_hosts::setup::Environment,
) -> Result<()> {
    protected_target(manager.home(), environment)
}
pub(super) fn protected_target(
    home: &Path,
    environment: &agentchat_hosts::setup::Environment,
) -> Result<()> {
    super::io::checked(home)?;
    if home.parent().is_none() {
        return Err(conflict("Refusing removal at the filesystem root."));
    }
    if let Some(user) = &environment.home {
        let user = resolve(user)?;
        if user.starts_with(home) {
            return Err(conflict(
                "Refusing removal at the user home or its ancestor.",
            ));
        }
    }
    let roots = environment.resolve()?;
    let mut protected = vec![
        roots
            .codex_config
            .parent()
            .expect("config parent")
            .to_owned(),
        roots
            .claude_settings
            .parent()
            .expect("settings parent")
            .to_owned(),
        roots
            .codex_skill
            .ancestors()
            .nth(2)
            .expect("skills root")
            .to_owned(),
    ];
    if let Some(root) = &environment.user_config_root {
        protected.push(root.clone());
    }
    for root in protected {
        let root = resolve(&root)?;
        if root.starts_with(home) {
            return Err(conflict(
                "Selected state root contains a protected host configuration root.",
            ));
        }
    }
    for target in [
        roots.codex_config,
        roots.codex_hooks,
        roots.claude_registry,
        roots.claude_settings,
        roots
            .codex_skill
            .parent()
            .expect("skill directory")
            .to_owned(),
        roots
            .claude_skill
            .parent()
            .expect("skill directory")
            .to_owned(),
    ] {
        let target = resolve(&target)?;
        if home.starts_with(&target) || target.starts_with(home) {
            return Err(conflict(
                "Removal overlaps a supported host integration destination; all targets were preserved.",
            ));
        }
    }
    Ok(())
}
pub(super) fn resolve(path: &Path) -> Result<PathBuf> {
    super::io::checked(path)?;
    let mut ancestor = path;
    while !ancestor.try_exists().map_err(io)? {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| conflict("Path has no existing parent."))?;
    }
    let mut value = ancestor.canonicalize().map_err(io)?;
    value.extend(
        path.strip_prefix(ancestor)
            .map_err(|_| conflict("Invalid native root."))?
            .components(),
    );
    Ok(value)
}
fn inactive(path: &Path) -> Result<()> {
    use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketType};
    #[cfg(target_os = "linux")]
    let fd = net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        net::SocketFlags::NONBLOCK | net::SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|e| io(e.into()))?;
    #[cfg(target_os = "macos")]
    let fd = {
        let fd =
            net::socket(AddressFamily::UNIX, SocketType::STREAM, None).map_err(|e| io(e.into()))?;
        rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC).map_err(|e| io(e.into()))?;
        let flags = rustix::fs::fcntl_getfl(&fd).map_err(|e| io(e.into()))?;
        rustix::fs::fcntl_setfl(&fd, flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(|e| io(e.into()))?;
        fd
    };
    let address = SocketAddrUnix::new(path).map_err(|_| busy())?;
    match net::connect(&fd, &address) {
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::CONNREFUSED) => Ok(()),
        _ => Err(busy()),
    }
}
fn busy() -> Error {
    let mut e = Error::new(
        ErrorCode::Busy,
        "A relevant AgentChat runtime or startup is active or uncertain. Stop it normally, then retry uninstall.",
    );
    e.retryable = true;
    e
}
