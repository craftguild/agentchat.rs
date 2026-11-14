//! Lossless durable catch-up from typed checkpoints, then coalesced head hints.

use super::{
    RuntimeOptions,
    connection::write,
    coordinator::{FollowKind, Service, Subscription},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    read::{ChangeRequest, requests},
};
use agentchat_protocol::rpc::{ChangeFrame, ChangeNotice};
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
    mut subscription: Subscription,
    options: RuntimeOptions,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let FollowKind::Changes { after } = subscription.kind else {
        unreachable!("typed change observer dispatch")
    };
    let mut cursor = after;
    let mut through = None;
    loop {
        let batch = match service
            .read(
                connection,
                None,
                requests::ViewerChanges {
                    changes: ChangeRequest {
                        after: cursor,
                        through,
                        limit: Some(200),
                    },
                },
                None,
            )
            .await
        {
            Ok(batch) => batch,
            Err(error) => {
                // Typed terminal errors preserve invalidation/reset semantics.
                // An error frame never claims another scanned/applied boundary.
                write(
                    writer,
                    &ChangeFrame {
                        jsonrpc: "2.0".into(),
                        notice: ChangeNotice::Error(error),
                    },
                    options,
                )
                .await?;
                return Ok(());
            }
        };
        let more = batch.has_more;
        let scanned = batch.scan;
        let head = batch.head;
        write(
            writer,
            &ChangeFrame {
                jsonrpc: "2.0".into(),
                notice: ChangeNotice::Batch(batch),
            },
            options,
        )
        .await?;
        cursor = scanned;
        through = more.then_some(head);
        if more {
            continue;
        }
        // Mark exactly this hint version observed before waiting. A newer
        // commit racing the following select still makes changed() ready.
        let hinted = *subscription.hints.borrow_and_update();
        if hinted > cursor.seq {
            continue;
        }
        tokio::select! {
            changed=subscription.hints.changed()=>if changed.is_err() {return Ok(());},
            _=shutdown.changed()=>return Ok(()),
            input=reader.fill_buf()=>{
                if input.map_err(|_|super::unavailable("Change observer read failed."))?.is_empty() {return Ok(());}
                return Err(Error::new(ErrorCode::InvalidArgument,"A change observer cannot send further commands."));
            }
        }
    }
}
