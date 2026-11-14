//! Explicit human actions use ordinary user participation without changing observer reads.
mod support;
use agentchat_core::{
    Result,
    read::{ChannelListCondition, requests},
};
use agentchat_runtime::{
    operations::{Launcher, Operations},
    project::Project,
};
use async_trait::async_trait;
use std::{path::Path, sync::Arc};
use support::runtime::{Running, join, options};
use uuid::Uuid;

struct NoLaunch;
#[async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        panic!("test runtime already owns this project")
    }
}

#[tokio::test]
async fn user_create_post_reply_retry_and_bounds_preserve_shared_policy() {
    let r = Running::new(options()).await;
    let operations = Operations::new(r.f.manager.clone(), Arc::new(NoLaunch));
    let viewer = operations.viewer(r.f.workspace.clone()).await.unwrap();
    let reader = viewer.reader();
    assert!(
        reader
            .agents(Default::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        reader
            .channel_members(requests::ChannelMembers {
                channel: "dev".into(),
                page: Default::default()
            })
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let writer = viewer.writer();
    let created_id = Uuid::new_v4();
    let channel = writer
        .create_channel(created_id, "review".into())
        .await
        .unwrap();
    assert_eq!(
        writer
            .create_channel(created_id, "review".into())
            .await
            .unwrap()
            .channel_id,
        channel.channel_id
    );
    assert_eq!(
        reader
            .channels(ChannelListCondition::All, Default::default())
            .await
            .unwrap()
            .page
            .items
            .len(),
        2
    );
    assert!(
        reader
            .agents(Default::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let members = reader
        .channel_members(requests::ChannelMembers {
            channel: "review".into(),
            page: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(members.items.len(), 1);
    let body = format!("x{}", "\0".repeat(65535));
    let id = Uuid::new_v4();
    let posted = writer
        .post(id, "review".into(), body.clone())
        .await
        .unwrap();
    assert_eq!(
        writer
            .post(id, "review".into(), body.clone())
            .await
            .unwrap()
            .message_id,
        posted.message_id
    );
    let message = reader
        .message(requests::Message {
            message_id: posted.message_id,
            receipts: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(message.message.message.body, body);
    assert_eq!(message.message.author.handle, "user");
    let reply = writer
        .reply(Uuid::new_v4(), posted.message_id, "A reply".into())
        .await
        .unwrap();
    assert!(
        writer
            .reply(Uuid::new_v4(), reply.message_id, "Nested".into())
            .await
            .is_err()
    );
    assert!(
        writer
            .post(Uuid::new_v4(), "review".into(), "x".repeat(65537))
            .await
            .is_err()
    );
    assert!(
        writer
            .post(id, "review".into(), "Changed".into())
            .await
            .is_err()
    );
    let health = r.client().await.health().await.unwrap();
    assert_eq!(health.application.statistics.messages, 2);
    assert_eq!(health.application.statistics.read_receipts, 0);
    assert!(
        serde_json::to_value(&health.application.statistics)
            .unwrap()
            .get("acknowledged_deliveries")
            .is_none()
    );
    viewer.close().await;
    r.stop().await;
}

#[tokio::test]
async fn user_post_freezes_ordinary_mention_warnings_without_creating_agent_identity() {
    let r = Running::new(options()).await;
    let context = r.context("receiver").await;
    join(&mut r.client().await, &context, "receiver").await;
    let viewer = Operations::new(r.f.manager.clone(), Arc::new(NoLaunch))
        .viewer(r.f.workspace.clone())
        .await
        .unwrap();
    let posted = viewer
        .writer()
        .post(
            Uuid::new_v4(),
            "dev".into(),
            "@missing and @receiver".into(),
        )
        .await
        .unwrap();
    assert!(posted.warnings.iter().any(|w| w.handle == "missing"));
    assert_eq!(
        viewer
            .reader()
            .agents(Default::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    // An ordinary shared user action still does not invent any native input receipt.
    assert_eq!(
        r.client()
            .await
            .health()
            .await
            .unwrap()
            .application
            .statistics
            .read_receipts,
        0
    );
    viewer.close().await;
    r.stop().await;
}
