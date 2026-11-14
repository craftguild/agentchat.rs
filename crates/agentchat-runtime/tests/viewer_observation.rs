//! Real context-free change sockets, injected reads and owned observer lifetime.
mod support;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    read::{MAX_PAGE_BYTES, ViewerChangeKind, requests},
};
use agentchat_protocol::rpc::success;
use agentchat_runtime::{
    operations::{Launcher, Operations},
    project::Project,
};
use async_trait::async_trait;
use std::{collections::BTreeSet, path::Path, sync::Arc, time::Duration};
use support::runtime::{Running, join, mutate, options};
use uuid::Uuid;

struct NoLaunch;
#[async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "Only the test's explicitly owned runtime may start.",
        ))
    }
}
fn backend(r: &Running) -> Operations {
    Operations::new(r.f.manager.clone(), Arc::new(NoLaunch))
}

#[tokio::test]
async fn project_participants_include_silent_and_left_identities_without_viewer_side_effects() {
    let r = Running::new(options()).await;
    let a = r.context("silent").await;
    let b = r.context("left").await;
    let mut client = r.client().await;
    let mut other = r.client().await;
    join(&mut client, &a, "silent").await;
    join(&mut other, &b, "left").await;
    mutate(&mut other, &b, Mutation::Leave).await;
    let viewer = backend(&r).viewer(r.f.workspace.clone()).await.unwrap();
    assert_eq!(viewer.directory(), r.f.workspace.as_path());
    let reader = viewer.reader();
    let first = reader
        .agents(agentchat_core::read::PageRequest {
            limit: Some(1),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].agent.handle, "silent");
    assert!(first.items[0].agent.joined);
    let second = reader
        .agents(agentchat_core::read::PageRequest {
            limit: Some(1),
            after: first.next_cursor.unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].agent.handle, "left");
    assert!(!second.items[0].agent.joined);
    assert!(second.next_cursor.is_none());
    let status = client.health().await.unwrap();
    assert_eq!(status.application.statistics.agents, 2);
    assert_eq!(status.application.statistics.messages, 0);
    assert_eq!(status.application.statistics.read_receipts, 0);
    assert!(
        serde_json::to_value(&status.application.statistics)
            .unwrap()
            .get("acknowledged_deliveries")
            .is_none()
    );
    viewer.close().await;
    r.stop().await;
}

#[tokio::test]
async fn replay_setup_overlap_coalesced_hints_and_maximum_bodies_keep_complete_typed_progress() {
    let r = Running::new(options()).await;
    let a = r.context("author").await;
    let mut sender = r.client().await;
    join(&mut sender, &a, "author").await;
    let after = success(
        r.client()
            .await
            .read(Uuid::new_v4(), None, requests::ViewerHead)
            .await
            .unwrap(),
    )
    .unwrap();
    let body = "\0".repeat(65_536);
    for _ in 0..4 {
        mutate(
            &mut sender,
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: body.clone(),
            },
        )
        .await;
    }
    let unicode = "💜".repeat(16_384);
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: unicode.clone(),
        },
    )
    .await;
    let mut observer = r.client().await.subscribe_changes(after).await.unwrap();
    let receiver = tokio::spawn(async move {
        let mut ids = BTreeSet::new();
        let mut bodies = Vec::new();
        let mut previous = after.seq;
        while bodies.len() < 70 {
            let batch = observer.next().await.unwrap();
            assert_eq!(batch.after.seq, previous);
            assert!(serde_json::to_vec(&batch).unwrap().len() <= MAX_PAGE_BYTES);
            for item in batch.items {
                assert!(ids.insert(item.event_id));
                let ViewerChangeKind::MessageCreated { message } = item.change else {
                    panic!("message source")
                };
                bodies.push(message.message.message.body);
            }
            previous = batch.scan.seq;
            assert_eq!(observer.checkpoint(), batch.scan);
        }
        bodies
    });
    for n in 0..65 {
        mutate(
            &mut sender,
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: format!("racing {n}"),
            },
        )
        .await;
    }
    let bodies = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .unwrap()
        .unwrap();
    assert!(bodies[..4].iter().all(|s| s == &body));
    assert_eq!(bodies[4], unicode);
    for n in 0..65 {
        assert_eq!(bodies[n + 5], format!("racing {n}"));
    }
    let health = sender.health().await.unwrap();
    assert_eq!(health.application.statistics.messages, 70);
    assert_eq!(health.application.statistics.agents, 1);
    assert_eq!(health.application.statistics.read_receipts, 0);
    assert!(
        serde_json::to_value(&health.application.statistics)
            .unwrap()
            .get("acknowledged_deliveries")
            .is_none()
    );
    r.stop().await;
}

#[tokio::test]
async fn context_free_lease_holds_idle_activity_and_close_leaves_ordinary_runtime_shutdown() {
    let mut opts = options();
    opts.idle = Duration::from_millis(200);
    let r = Running::new(opts).await;
    let viewer = backend(&r).viewer(r.f.workspace.clone()).await.unwrap();
    let reader = viewer.reader();
    let head = reader.head().await.unwrap();
    assert_eq!(head.project_id, r.project.identity().id);
    let roots = reader
        .roots(requests::RootWindow {
            channel: "dev".into(),
            window: Default::default(),
        })
        .await
        .unwrap();
    assert!(roots.items.is_empty());
    tokio::time::sleep(Duration::from_millis(450)).await;
    assert!(
        !r.finished(),
        "a viewer lease is runtime activity without participation"
    );
    let status = r.client().await.health().await.unwrap();
    assert_eq!(status.runtime.activity.observers, 1);
    assert_eq!(status.runtime.activity.attached_connections, 0);
    assert_eq!(status.application.statistics.agents, 0);
    assert_eq!(status.runtime.native_delivery.jobs, 0);
    viewer.close().await;
    // The retained reader has no participant/heartbeat or always-open query
    // socket. Closing its lease restores the runtime's ordinary idle lifetime.
    tokio::time::timeout(Duration::from_secs(3), async {
        while !r.finished() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(reader);
    r.stop().await;
}

#[tokio::test]
async fn injected_observer_reconnects_same_project_and_replays_after_actual_runtime_restart() {
    let mut r = Running::new(options()).await;
    let viewer = backend(&r).viewer(r.f.workspace.clone()).await.unwrap();
    let reader = viewer.reader();
    let after = reader.head().await.unwrap();
    let mut observer = reader.watch(after).await.unwrap();
    assert!(observer.next().await.unwrap().items.is_empty());
    r.restart(options()).await;
    let a = r.context("new writer").await;
    let mut sender = r.client().await;
    join(&mut sender, &a, "author").await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "restart gap".into(),
        },
    )
    .await;
    let batch = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let batch = observer.next().await.unwrap();
            if !batch.items.is_empty() {
                break batch;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(batch.scan.project_id, after.project_id);
    assert!(batch.scan.seq > after.seq);
    let ViewerChangeKind::MessageCreated { message } = &batch.items[0].change else {
        panic!("replayed body")
    };
    assert_eq!(message.message.message.body, "restart gap");
    let state = sender.health().await.unwrap();
    assert_eq!(state.application.statistics.agents, 1);
    assert_eq!(state.application.statistics.read_receipts, 0);
    assert!(
        serde_json::to_value(&state.application.statistics)
            .unwrap()
            .get("acknowledged_deliveries")
            .is_none()
    );
    drop(observer);
    viewer.close().await;
    r.stop().await;
}

#[tokio::test]
async fn observation_refuses_a_participant_connection_and_invalidated_start_has_typed_error() {
    let r = Running::new(options()).await;
    let a = r.context("participant").await;
    let mut participant = r.client().await;
    join(&mut participant, &a, "author").await;
    let mut unbound = r.client().await;
    let after = success(
        unbound
            .read(Uuid::new_v4(), None, requests::ViewerHead)
            .await
            .unwrap(),
    )
    .unwrap();
    drop(unbound);
    let error = match participant.subscribe_changes(after).await {
        Err(e) => e,
        Ok(_) => panic!("bound observer"),
    };
    assert_eq!(error.code, ErrorCode::IdentityConflict);
    let mut wrong = after;
    wrong.event_id = Some(Uuid::new_v4());
    let error = match r.client().await.subscribe_changes(wrong).await {
        Err(e) => e,
        Ok(_) => panic!("invalidated start"),
    };
    assert_eq!(error.details["reset_required"], true);
    let viewer = backend(&r).viewer(r.f.workspace.clone()).await.unwrap();
    let reader = viewer.reader();
    // Namespace replacement is performed after the test's owned runtime exits.
    // The still-open viewer must retain its captured namespace across that loss.
    r.handle.shutdown();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !r.finished() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut replace = r.f.request();
    replace.new_project = true;
    let new =
        r.f.manager
            .initialize(&r.f.workspace, replace)
            .await
            .unwrap();
    assert_ne!(new.project.identity().id, reader.project_id());
    assert_eq!(
        reader.head().await.unwrap_err().code,
        ErrorCode::ProjectBindingMismatch
    );
    viewer.close().await;
    r.stop().await;
}
