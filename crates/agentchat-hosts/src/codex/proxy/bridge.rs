//! Byte-transparent full-duplex relay with bounded, passive lifecycle observation.
use super::{
    frame_tap::{Packet, Tap},
    observation::Observer,
};
use crate::codex::{required, unavailable};
use agentchat_core::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};
use tokio_tungstenite::tungstenite::Message;
pub(super) async fn bridge(
    front: UnixStream,
    backend: &std::path::Path,
    connection: u64,
    observer: &Observer,
) -> Result<()> {
    // A failed new backend connection must not revoke healthy sibling streams.
    if !super::backend_socket(backend)? {
        return Err(required());
    }
    let back = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        UnixStream::connect(backend),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    if back.peer_cred().map_err(|_| required())?.uid() != rustix::process::geteuid().as_raw() {
        return Err(required());
    }
    let (front_read, front_write) = front.into_split();
    let (back_read, back_write) = back.into_split();
    // Independent reads/writes preserve native backpressure and half-close.
    // No WebSocket termination, negotiated-header rewrite or automatic Pong.
    tokio::try_join!(
        pump(front_read, back_write, true, connection, observer),
        pump(back_read, front_write, false, connection, observer)
    )?;
    Ok(())
}
async fn pump(
    mut read: OwnedReadHalf,
    mut write: OwnedWriteHalf,
    frontend: bool,
    connection: u64,
    observer: &Observer,
) -> Result<()> {
    let mut tap = Tap::new(frontend);
    let mut bytes = [0u8; 8192];
    loop {
        let n = read.read(&mut bytes).await.map_err(|_| unavailable())?;
        if n == 0 {
            write.shutdown().await.map_err(|_| unavailable())?;
            return Ok(());
        }
        if !observer.enabled() {
            tap.disable();
            write
                .write_all(&bytes[..n])
                .await
                .map_err(|_| unavailable())?;
            continue;
        }
        let mut start = 0;
        let observe = frontend || observer.pending(connection);
        for (i, byte) in bytes[..n].iter().copied().enumerate() {
            if tap.byte(byte, observe) {
                let packet = tap.finish().await;
                let response = match packet {
                    Packet::Metadata(metadata) => {
                        if frontend {
                            observer.request(connection, &metadata).await?;
                            None
                        } else {
                            observer.response(connection, &metadata)
                        }
                    }
                    Packet::Unobservable => {
                        observer
                            .request(connection, &Message::Binary(Vec::new().into()))
                            .await?;
                        None
                    }
                    Packet::Ignored => None,
                };
                write
                    .write_all(&bytes[start..=i])
                    .await
                    .map_err(|_| unavailable())?;
                if let Some(response) = response {
                    observer.forwarded(response).await;
                }
                start = i + 1;
            }
        }
        if !tap.flush().await {
            observer
                .request(connection, &Message::Binary(Vec::new().into()))
                .await?;
        }
        write
            .write_all(&bytes[start..n])
            .await
            .map_err(|_| unavailable())?;
    }
}
