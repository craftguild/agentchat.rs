//! Readiness-driven host pipes allow cancellation without waiting for EOF.

use std::{
    fs::File,
    os::{fd::AsFd, unix::fs::FileTypeExt},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::unix::pipe::{Receiver, Sender},
};

type Reader = Box<dyn AsyncRead + Unpin + Send>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;

pub(crate) fn streams() -> std::io::Result<(Reader, Writer)> {
    let input = File::from(std::io::stdin().as_fd().try_clone_to_owned()?);
    let output = File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
    // Tokio's ordinary stdio uses blocking worker threads. Cancelling their
    // futures cannot stop a read on a host-held pipe or an unwritable output,
    // so runtime shutdown would otherwise outlive our transport deadlines.
    // Only pipes are made nonblocking; terminal/file behavior stays unchanged.
    let reader: Reader = if input.metadata()?.file_type().is_fifo() {
        Box::new(Receiver::from_file(input)?)
    } else {
        Box::new(tokio::io::stdin())
    };
    let writer: Writer = if output.metadata()?.file_type().is_fifo() {
        Box::new(Sender::from_file(output)?)
    } else {
        Box::new(tokio::io::stdout())
    };
    Ok((reader, writer))
}
