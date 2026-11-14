//! Owned integration sockets; checking them never invents host liveness.

use super::Operations;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::{
    operations::{Attachment, Binding, Target},
    rpc::{Call, Client, success},
};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use uuid::Uuid;

struct Attached {
    binding: Binding,
    client: Arc<Mutex<Option<Client>>>,
    heartbeat: JoinHandle<()>,
}
impl Drop for Attached {
    fn drop(&mut self) {
        // The task must never retain a socket after its host-owned attachment.
        self.heartbeat.abort();
    }
}
fn disconnected() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The owned integration connection has ended.",
    )
}
async fn check(client: &mut Client, context_id: Uuid) -> Result<()> {
    let value = client.native_binding(context_id).await?;
    if value.agent.is_none_or(|a| !a.connected || !a.agent.joined) {
        return Err(disconnected());
    }
    Ok(())
}
async fn refresh(client: &mut Client, context_id: Uuid) -> Result<()> {
    // Check the retained participant before obtaining fresh host evidence.
    // In particular, an explicit leave or expired incarnation is terminal.
    check(client, context_id).await?;
    success(
        client
            .call(Uuid::new_v4(), Call::Heartbeat { context_id })
            .await?,
    )?;
    Ok(())
}
fn heartbeat(client: Arc<Mutex<Option<Client>>>, context_id: Uuid) -> JoinHandle<()> {
    tokio::spawn(async move {
        let period = Duration::from_secs(15);
        let mut ticks = tokio::time::interval_at(Instant::now() + period, period);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let mut connection = client.lock().await;
            let Some(active) = connection.as_mut() else {
                return;
            };
            // The runtime's shared heartbeat operation obtains new HostGate
            // evidence. This timer/socket alone never establishes presence.
            let outcome = refresh(active, context_id).await;
            if let Err(error) = outcome {
                if error.retryable && matches!(error.code, ErrorCode::Busy) {
                    continue;
                }
                connection.take();
                return;
            }
        }
    })
}
#[async_trait]
impl Attachment for Attached {
    fn binding(&self) -> &Binding {
        &self.binding
    }
    async fn check(&self) -> Result<()> {
        let mut owned = self.client.lock().await;
        let client = owned.as_mut().ok_or_else(disconnected)?;
        check(client, self.binding.context_id).await
    }
    async fn refresh(&self) -> Result<()> {
        let mut owned = self.client.lock().await;
        let client = owned.as_mut().ok_or_else(disconnected)?;
        let outcome = refresh(client, self.binding.context_id).await;
        if let Err(error) = &outcome
            && !(error.retryable && error.code == ErrorCode::Busy)
        {
            // Loss of authority also closes the timer's shared socket. A
            // later hint cannot recover it; lifecycle restoration is separate.
            owned.take();
        }
        outcome
    }
}
impl Operations {
    pub(super) async fn attachment(&self, target: Target) -> Result<Box<dyn Attachment>> {
        let route = self.route(&target).await?;
        let context = route.context.as_ref().ok_or_else(|| {
            Error::new(
                ErrorCode::SessionRequired,
                "Restoration requires an exact context.",
            )
        })?;
        let mut client = self.connect(&route).await?;
        let value = success(
            client
                .call(
                    Uuid::new_v4(),
                    Call::Attach {
                        context_id: context.context_id,
                    },
                )
                .await?,
        )?;
        if value["agent"]["joined"] != true {
            return Err(Error::new(
                ErrorCode::NotJoined,
                "This conversation has not joined; restoration never invites it.",
            ));
        }
        let agent_id = serde_json::from_value(value["agent"]["id"].clone()).map_err(|_| {
            Error::new(
                ErrorCode::IntegrityError,
                "Restored identity is missing its agent UUID.",
            )
        })?;
        let client = Arc::new(Mutex::new(Some(client)));
        let heartbeat = heartbeat(client.clone(), context.context_id);
        Ok(Box::new(Attached {
            binding: Binding {
                project_id: route.project.identity().id,
                context_id: context.context_id,
                agent_id,
            },
            client,
            heartbeat,
        }))
    }
}
