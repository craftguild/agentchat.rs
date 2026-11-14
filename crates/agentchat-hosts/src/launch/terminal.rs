//! Reopen the actual controlling device, preserving native event-loop support.
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    ffi::OsStr,
    os::{fd::AsFd, unix::ffi::OsStrExt},
    process::Stdio,
};

pub(super) fn controlling_stdio() -> Result<[Stdio; 3]> {
    let session = rustix::process::getsid(None).map_err(|_| required())?;
    let input = std::io::stdin();
    let output = std::io::stdout();
    let error = std::io::stderr();
    for fd in [input.as_fd(), output.as_fd(), error.as_fd()] {
        // tcgetsid succeeds only for this session's controlling terminal.
        // A redirected descriptor or unrelated terminal must never be selected.
        if rustix::termios::tcgetsid(fd).ok() != Some(session) {
            continue;
        }
        let Ok(name) = rustix::termios::ttyname(fd, Vec::new()) else {
            continue;
        };
        if name.to_bytes() == b"/dev/tty" {
            continue;
        }
        // macOS accepts isatty(/dev/tty) but rejects its kqueue registration.
        // Native Bun/Node loops need the actual device (e.g. /dev/ttys001).
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(OsStr::from_bytes(name.to_bytes()))
            .map_err(|_| required())?;
        if rustix::termios::tcgetsid(&tty).ok() != Some(session) {
            return Err(required());
        }
        return Ok([
            Stdio::from(tty.try_clone().map_err(|_| required())?),
            Stdio::from(tty.try_clone().map_err(|_| required())?),
            Stdio::from(tty),
        ]);
    }
    Err(required())
}
fn required() -> Error {
    Error::new(
        ErrorCode::IoError,
        "Native launch requires an attached controlling terminal.",
    )
}
