//! A single nonblocking kernel probe never sends a host/application request.
use super::unavailable;
use agentchat_core::Result;
use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketType};
use std::path::Path;
pub(crate) fn socket(path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    let fd = net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        rustix::net::SocketFlags::NONBLOCK | rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|_| unavailable())?;
    #[cfg(target_os = "macos")]
    let fd = {
        let fd = net::socket(AddressFamily::UNIX, SocketType::STREAM, None)
            .map_err(|_| unavailable())?;
        rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC).map_err(|_| unavailable())?;
        let flags = rustix::fs::fcntl_getfl(&fd).map_err(|_| unavailable())?;
        rustix::fs::fcntl_setfl(&fd, flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(|_| unavailable())?;
        fd
    };
    let address = SocketAddrUnix::new(path).map_err(|_| unavailable())?;
    match net::connect(&fd, &address) {
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::CONNREFUSED) => Ok(()),
        _ => Err(unavailable()),
    }
}
