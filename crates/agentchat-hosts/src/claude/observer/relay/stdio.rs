//! Cancellable owned pipe I/O; blocking standard-input tasks outlive timeouts.
use crate::claude::freshness::Snapshot;
use agentchat_core::{Error, ErrorCode, Result};
use rustix::fs::{FileType, OFlags};
use std::{
    io,
    os::fd::{AsFd, OwnedFd},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    net::{UnixStream, unix::pipe},
    time::timeout,
};

struct Restore {
    fd: OwnedFd,
    flags: OFlags,
}
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = rustix::fs::fcntl_setfl(&self.fd, self.flags);
    }
}
fn descriptor(fd: impl AsFd) -> Result<(OwnedFd, FileType, Restore)> {
    let fd = rustix::io::dup(fd).map_err(|_| invalid("Native pipe is unavailable."))?;
    let stat =
        rustix::fs::fstat(&fd).map_err(|_| invalid("Native pipe metadata is unavailable."))?;
    let kind = FileType::from_raw_mode(stat.st_mode);
    if !matches!(kind, FileType::Fifo | FileType::Socket) {
        return Err(invalid(
            "Native observation requires a dedicated pipe or socket.",
        ));
    }
    let flags =
        rustix::fs::fcntl_getfl(&fd).map_err(|_| invalid("Native pipe flags are unavailable."))?;
    let backup =
        rustix::io::dup(&fd).map_err(|_| invalid("Native pipe flags cannot be retained."))?;
    Ok((fd, kind, Restore { fd: backup, flags }))
}
fn socket(fd: OwnedFd) -> Result<UnixStream> {
    let stream = std::os::unix::net::UnixStream::from(fd);
    stream
        .set_nonblocking(true)
        .map_err(|_| invalid("Native socket cannot become nonblocking."))?;
    UnixStream::from_std(stream).map_err(|_| invalid("Native socket readiness is unavailable."))
}
struct Output {
    inner: Box<dyn AsyncWrite + Unpin + Send>,
    _restore: Restore,
}
impl AsyncWrite for Output {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Open only native stdout's dedicated pipe/socket for cancellable writes.
/// Terminals and regular files are refused; original descriptor flags restore
/// on drop. The native observation helper must never alter a parent TTY mode.
pub fn native_output() -> Result<impl AsyncWrite + Unpin> {
    let (fd, kind, restore) = descriptor(std::io::stdout())?;
    let inner: Box<dyn AsyncWrite + Unpin + Send> = if kind == FileType::Fifo {
        Box::new(
            pipe::Sender::from_owned_fd(fd)
                .map_err(|_| invalid("Native output pipe readiness is unavailable."))?,
        )
    } else {
        Box::new(socket(fd)?)
    };
    Ok(Output {
        inner,
        _restore: restore,
    })
}

/// Read one bounded EOF-terminated native answer within two seconds.
/// Cancelling native pipe readiness leaves no blocking stdin worker to prevent
/// process shutdown, and restores the original pipe flags before returning.
pub async fn read_answer() -> Result<Snapshot> {
    let bytes = read_input(8192).await?;
    serde_json::from_slice(&bytes).map_err(|_| invalid("Native answer has an invalid format."))
}

/// Read bounded EOF-terminated native callback JSON through dedicated cancellable
/// pipes/sockets. Terminals, regular files and bounds above 64 KiB are refused.
/// No blocking stdin worker survives a timeout or native callback cancellation.
pub async fn read_input(limit: usize) -> Result<Vec<u8>> {
    if limit == 0 || limit > 65_536 {
        return Err(invalid("Native input bound is invalid."));
    }
    let bytes = read_bounded(limit).await?;
    if bytes.len() > limit {
        return Err(invalid("Native callback input exceeds its bound."));
    }
    Ok(bytes)
}

/// Read at most one MiB of native hook JSON plus its overflow marker. Normal
/// prompts can pass without opening agentchat state; callers classify truncated owned
/// notifications conservatively. Pipe readiness remains cancellable and finite.
pub async fn read_hook_input() -> Result<(Vec<u8>, bool)> {
    let bytes = read_bounded(1_048_576).await?;
    let oversized = bytes.len() > 1_048_576;
    Ok((bytes, oversized))
}
async fn read_bounded(limit: usize) -> Result<Vec<u8>> {
    let (fd, kind, _restore) = descriptor(std::io::stdin())?;
    let input: Box<dyn AsyncRead + Unpin + Send> = if kind == FileType::Fifo {
        Box::new(
            pipe::Receiver::from_owned_fd(fd)
                .map_err(|_| invalid("Native input pipe readiness is unavailable."))?,
        )
    } else {
        Box::new(socket(fd)?)
    };
    let mut bytes = Vec::new();
    timeout(
        Duration::from_secs(2),
        input.take((limit + 1) as u64).read_to_end(&mut bytes),
    )
    .await
    .map_err(|_| invalid("Native answer input timed out."))?
    .map_err(|_| invalid("Native answer input is unavailable."))?;
    Ok(bytes)
}
fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}
