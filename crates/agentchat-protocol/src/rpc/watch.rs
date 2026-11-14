//! Typed change-watch setup, incremental frames and a context-free activity lease.

use super::{Call, Client, invalid, success, unavailable};
use crate::viewer::ChangeObserver;
use agentchat_core::{
    Error, Result,
    read::{ChangeBatch, Checkpoint, MAX_PAGE_BYTES},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Validated retained-change setup, after registering committed-head hints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeStart {
    /// Explicit requested replay boundary.
    pub after: Checkpoint,
    /// Committed boundary observed during setup.
    pub head: Checkpoint,
}

/// Context-free runtime activity lease, without participant or native heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserverStart {
    /// Fixed project and committed history observed during registration.
    pub head: Checkpoint,
}

/// Typed notification discriminant; it never guesses from overlapping payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ChangeNotice {
    /// Complete retained projection and safe scanned progress.
    #[serde(rename = "viewer.changes")]
    Batch(ChangeBatch),
    /// Terminal shared source error; no progress is implied.
    #[serde(rename = "viewer.error")]
    Error(Error),
}

/// Version-1 JSON-RPC change notification on a dedicated connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeFrame {
    /// Exact JSON-RPC version string.
    pub jsonrpc: String,
    /// Explicitly tagged batch or terminal error.
    #[serde(flatten)]
    pub notice: ChangeNotice,
}

/// An owned context-free stream, with last-complete progress and no body queue.
pub struct ChangeFollower {
    client: Client,
    cursor: Checkpoint,
    failed: bool,
    through: Option<Checkpoint>,
}

impl Client {
    /// Turn this unbound connection into a context-free retained-change stream.
    pub async fn subscribe_changes(mut self, after: Checkpoint) -> Result<ChangeFollower> {
        let start = success(
            self.call_as::<ChangeStart>(Uuid::new_v4(), Call::SubscribeChanges { after })
                .await?,
        )?;
        if start.after != after
            || start.after.project_id != self.project
            || !well_formed(start.after)
            || start.head.project_id != self.project
            || start.head.seq < after.seq
            || !well_formed(start.head)
        {
            return Err(invalid(
                "Change subscription setup disagrees with its request.",
            ));
        }
        Ok(ChangeFollower {
            client: self,
            cursor: after,
            failed: false,
            through: None,
        })
    }

    /// Hold runtime activity without a participant, body stream or heartbeat.
    pub async fn observe(mut self) -> Result<ObserverLease> {
        let start = success(
            self.call_as::<ObserverStart>(Uuid::new_v4(), Call::Observe)
                .await?,
        )?;
        if start.head.project_id != self.project || !well_formed(start.head) {
            return Err(invalid("Observer lease belongs to another history."));
        }
        Ok(ObserverLease {
            client: self,
            head: start.head,
        })
    }
}

impl ChangeFollower {
    /// Last completely received progress, independent of browser application.
    pub fn checkpoint(&self) -> Checkpoint {
        self.cursor
    }
    /// Receive one complete batch; partial-frame cancellation preserves bytes.
    pub async fn next(&mut self) -> Result<ChangeBatch> {
        if self.failed {
            return Err(unavailable("Reconnect a failed change observer."));
        }
        let result = async {
            let frame: ChangeFrame = self.client.reader.read().await?.ok_or_else(|| {
                unavailable("Change observer closed; replay from its last complete checkpoint.")
            })?;
            if frame.jsonrpc != "2.0" {
                return Err(invalid("Invalid change notification version."));
            }
            match frame.notice {
                ChangeNotice::Error(error) => Err(error),
                ChangeNotice::Batch(batch) => {
                    validate(&batch, self.cursor)?;
                    if self.through.is_some_and(|head| head != batch.head) {
                        return Err(invalid("Change continuation changed its captured head."));
                    }
                    Ok(batch)
                }
            }
        }
        .await;
        match result {
            Ok(batch) => {
                self.cursor = batch.scan;
                self.through = batch.has_more.then_some(batch.head);
                Ok(batch)
            }
            Err(mut error) => {
                self.failed = true;
                error.details["last_checkpoint"] = serde_json::json!(self.cursor);
                Err(error)
            }
        }
    }
}

#[async_trait]
impl ChangeObserver for ChangeFollower {
    fn checkpoint(&self) -> Checkpoint {
        self.cursor
    }
    async fn next(&mut self) -> Result<ChangeBatch> {
        ChangeFollower::next(self).await
    }
}

/// No queued events and no native lifecycle: dropping closes only this lease.
pub struct ObserverLease {
    client: Client,
    head: Checkpoint,
}
impl ObserverLease {
    /// Snapshot metadata at lease acquisition, never a delivery/read boundary.
    pub fn head(&self) -> Checkpoint {
        self.head
    }
    /// Wait for runtime loss without polling or holding a database snapshot.
    pub async fn closed(&mut self) -> Result<()> {
        let frame: Option<()> = self.client.reader.read().await?;
        if frame.is_some() {
            return Err(invalid("An observer lease cannot receive data frames."));
        }
        Err(unavailable("Runtime observer lease closed."))
    }
}

fn well_formed(cursor: Checkpoint) -> bool {
    (cursor.seq == agentchat_core::sequence::Cursor::START) == cursor.event_id.is_none()
}
fn validate(batch: &ChangeBatch, after: Checkpoint) -> Result<()> {
    if batch.after != after
        || batch.scan.project_id != after.project_id
        || batch.head.project_id != after.project_id
        || !well_formed(batch.scan)
        || !well_formed(batch.head)
        || batch.scan.seq < after.seq
        || batch.scan.seq > batch.head.seq
        || batch.has_more != (batch.scan.seq < batch.head.seq)
        || (batch.has_more && batch.scan.seq == after.seq)
        || batch.items.len() > 200
        || serde_json::to_vec(batch)
            .map_err(|_| invalid("Invalid change batch encoding."))?
            .len()
            > MAX_PAGE_BYTES
    {
        return Err(invalid(
            "Change batch has invalid project, range, progress or bounds.",
        ));
    }
    let mut previous = after.seq;
    for item in &batch.items {
        if item.seq.cursor() <= previous
            || item.seq.cursor() > batch.scan.seq
            || (item.seq.cursor() == batch.scan.seq && Some(item.event_id) != batch.scan.event_id)
        {
            return Err(invalid("Change items disagree with ordered scan progress."));
        }
        previous = item.seq.cursor();
    }
    Ok(())
}
