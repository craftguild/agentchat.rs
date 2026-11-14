//! Runtime-backed injected viewer reads and an owned context-free activity lease.

use super::{Operations, Route};
use agentchat_core::{
    Error, ErrorCode, Result,
    read::{
        ActorView, AgentView, ChangeBatch, ChangeRequest, ChannelListCondition, ChannelPage,
        Checkpoint, MessageDetail, MessageWindow, Page, PageRequest, PinnedRoot, ReadRequest,
        requests,
    },
};
use agentchat_protocol::{
    health::RuntimeWhoami,
    operations::Target,
    rpc::{ChangeFollower, Client, ObserverLease, success},
    viewer::{ChangeObserver, ViewerReader},
};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::task::JoinHandle;
use uuid::Uuid;

#[path = "viewer_writes.rs"]
mod writes;

/// Viewer-owned runtime attachment. Closing it never shuts down the runtime or
/// native hosts; their ordinary independent lifecycle continues after disconnect.
pub struct ViewerSession {
    reader: Arc<RuntimeViewer>,
    lease: Option<JoinHandle<()>>,
    _activity: agentchat_hosts::lifecycle::Activity,
}
impl ViewerSession {
    /// Canonical directory captured during authorized viewer routing; display
    /// metadata must not re-resolve or select another project after attachment.
    pub fn directory(&self) -> &std::path::Path {
        &self.reader.route.directory
    }
    /// Typed injected capability, with no storage/native adapter dependency.
    pub fn reader(&self) -> Arc<dyn ViewerReader> {
        self.reader.clone()
    }
    /// Explicit fixed-project user actions; constructing this capability does not join.
    pub fn writer(&self) -> Arc<dyn agentchat_protocol::viewer_write::ViewerWriter> {
        self.reader.clone()
    }
    /// Cancel and join only this viewer's lease task; retain ordinary runtime life.
    pub async fn close(mut self) {
        if let Some(task) = self.lease.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for ViewerSession {
    fn drop(&mut self) {
        if let Some(task) = &self.lease {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct RuntimeViewer {
    operations: Operations,
    route: Arc<Route>,
}
impl Operations {
    /// Open one initialized project as a context-free viewer. The caller passes
    /// a discovery path, never an agent descriptor or inherited participation.
    pub async fn viewer(&self, directory: PathBuf) -> Result<ViewerSession> {
        let route = self
            .route(&Target {
                project_path: Some(directory),
                ..Default::default()
            })
            .await?;
        let activity = self.manager.activity(route.project.identity().id).await?;
        self.manager.validate(&route.project).await?;
        let reader = Arc::new(RuntimeViewer {
            operations: self.clone(),
            route: Arc::new(route),
        });
        let initial = reader.connect().await?.observe().await?;
        let keep = reader.clone();
        let task = tokio::spawn(async move { hold(keep, initial).await });
        Ok(ViewerSession {
            reader,
            lease: Some(task),
            _activity: activity,
        })
    }
}
impl RuntimeViewer {
    async fn connect(&self) -> Result<Client> {
        self.operations.connect(&self.route).await
    }
    async fn read<R: ReadRequest>(&self, request: R) -> Result<R::Response<RuntimeWhoami>>
    where
        R::Response<RuntimeWhoami>: DeserializeOwned,
    {
        success(
            self.connect()
                .await?
                .read(Uuid::new_v4(), None, request)
                .await?,
        )
    }
}
#[async_trait]
impl ViewerReader for RuntimeViewer {
    fn project_id(&self) -> Uuid {
        self.route.project.identity().id
    }
    async fn head(&self) -> Result<Checkpoint> {
        self.read(requests::ViewerHead).await
    }
    async fn channels(
        &self,
        condition: ChannelListCondition,
        page: PageRequest,
    ) -> Result<ChannelPage> {
        self.read(requests::Channels { condition, page }).await
    }
    async fn agents(&self, page: PageRequest) -> Result<Page<AgentView>> {
        self.read(requests::Agents { page }).await
    }
    async fn channel_members(&self, request: requests::ChannelMembers) -> Result<Page<ActorView>> {
        self.read(request).await
    }
    async fn message(&self, request: requests::Message) -> Result<MessageDetail> {
        self.read(request).await
    }
    async fn roots(&self, request: requests::RootWindow) -> Result<MessageWindow> {
        self.read(request).await
    }
    async fn pinned(&self, request: requests::PinnedRoot) -> Result<PinnedRoot> {
        self.read(request).await
    }
    async fn replies(&self, request: requests::ReplyWindow) -> Result<MessageWindow> {
        self.read(request).await
    }
    async fn changes(&self, changes: ChangeRequest) -> Result<ChangeBatch> {
        self.read(requests::ViewerChanges { changes }).await
    }
    async fn watch(&self, after: Checkpoint) -> Result<Box<dyn ChangeObserver>> {
        let follower = self.connect().await?.subscribe_changes(after).await?;
        Ok(Box::new(ReconnectingObserver {
            reader: self.clone(),
            follower: Some(follower),
            cursor: after,
            attempt: 0,
        }))
    }
}

struct ReconnectingObserver {
    reader: RuntimeViewer,
    follower: Option<ChangeFollower>,
    cursor: Checkpoint,
    attempt: usize,
}
#[async_trait]
impl ChangeObserver for ReconnectingObserver {
    fn checkpoint(&self) -> Checkpoint {
        self.cursor
    }
    async fn next(&mut self) -> Result<ChangeBatch> {
        loop {
            if self.follower.is_none() {
                match self.reader.connect().await {
                    Ok(client) => match client.subscribe_changes(self.cursor).await {
                        Ok(follower) => self.follower = Some(follower),
                        Err(error) if transient(&error) => {
                            backoff(&mut self.attempt).await;
                            continue;
                        }
                        Err(error) => return Err(error),
                    },
                    Err(error) if transient(&error) => {
                        backoff(&mut self.attempt).await;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            match self.follower.as_mut().expect("owned observer").next().await {
                Ok(batch) => {
                    if batch.scan.seq > self.cursor.seq {
                        self.attempt = 0;
                    }
                    self.cursor = batch.scan;
                    return Ok(batch);
                }
                Err(error) if transient(&error) => {
                    // Retain only the last complete typed boundary. A partial
                    // frame cannot advance replay, and no browser ACK is inferred.
                    self.follower = None;
                    backoff(&mut self.attempt).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

async fn hold(reader: Arc<RuntimeViewer>, mut lease: ObserverLease) {
    loop {
        if lease.closed().await.is_err_and(|e| !transient(&e)) {
            return;
        }
        let mut attempt = 0;
        loop {
            backoff(&mut attempt).await;
            match reader.connect().await {
                Ok(client) => match client.observe().await {
                    Ok(next) => {
                        lease = next;
                        break;
                    }
                    Err(error) if !transient(&error) => return,
                    Err(_) => {}
                },
                Err(error) if !transient(&error) => return,
                Err(_) => {}
            }
        }
    }
}
fn transient(error: &Error) -> bool {
    matches!(
        error.code,
        ErrorCode::RuntimeUnavailable | ErrorCode::IoError | ErrorCode::Busy
    )
}
async fn backoff(attempt: &mut usize) {
    let seconds = [1, 2, 5, 10][(*attempt).min(3)];
    *attempt = attempt.saturating_add(1);
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}
