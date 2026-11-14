//! Durable observer catch-up; a single coalesced hint replaces event-body queues.

use super::{
    RuntimeOptions,
    coordinator::{FollowKind, Service, Subscription},
};
use agentchat_core::{Error, ErrorCode, Result};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::unix::{OwnedReadHalf, OwnedWriteHalf},
    sync::watch,
};
use uuid::Uuid;

pub(super) async fn run(
    reader: &mut BufReader<OwnedReadHalf>,
    writer: &mut OwnedWriteHalf,
    service: &Service,
    connection: Uuid,
    subscription: Subscription,
    options: RuntimeOptions,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    if matches!(subscription.kind, FollowKind::Changes { .. }) {
        return super::change_follow::run(
            reader,
            writer,
            service,
            connection,
            subscription,
            options,
            shutdown,
        )
        .await;
    }
    idle(reader, shutdown).await
}

async fn idle(
    reader: &mut BufReader<OwnedReadHalf>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    tokio::select! {
        _=shutdown.changed()=>Ok(()),
        input=reader.fill_buf()=>{
            if input.map_err(|_|super::unavailable("Observer lease read failed."))?.is_empty() {Ok(())}
            else {Err(Error::new(ErrorCode::InvalidArgument,"An observer lease cannot send further commands."))}
        }
    }
}
