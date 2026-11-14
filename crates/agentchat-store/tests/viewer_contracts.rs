//! Adapter budget cuts and readonly state checks use actual retained SQLite data.
mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    ports::Storage,
    read::{ChangeRequest, Checkpoint, WindowRequest, requests},
    time::Clock,
};
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn presentation_cuts_keep_omitted_roots_and_changes_reachable_with_the_original_head() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let initial = f.app.read(None, requests::ViewerHead).await.unwrap();
    let mut ids = Vec::new();
    for n in 0..8 {
        ids.push(id(
            &f.post(&a, &format!("body {n}")).await.result,
            "message_id",
        ));
    }
    let original = f
        .app
        .read(
            None,
            requests::RootWindow {
                channel: "dev".into(),
                window: Default::default(),
            },
        )
        .await
        .unwrap();
    let mut shortened = original.clone();
    shortened.retain_latest(2).unwrap();
    assert_eq!(shortened.snapshot, original.snapshot);
    assert_eq!(
        shortened
            .items
            .iter()
            .map(|m| m.message.message.id)
            .collect::<Vec<_>>(),
        ids[6..]
    );
    let older = f
        .app
        .read(
            None,
            requests::RootWindow {
                channel: "dev".into(),
                window: WindowRequest {
                    snapshot: Some(shortened.snapshot),
                    before: shortened.next_before,
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        older
            .items
            .iter()
            .map(|m| m.message.message.id)
            .collect::<Vec<_>>(),
        ids[..6]
    );
    let unchanged = shortened.clone();
    assert!(shortened.retain_latest(0).is_err());
    assert_eq!(shortened, unchanged);
    let all = f
        .app
        .read(
            None,
            requests::ViewerChanges {
                changes: ChangeRequest {
                    after: initial,
                    through: None,
                    limit: None,
                },
            },
        )
        .await
        .unwrap();
    let (prefix, rest) = all.clone().split_prefix(3).unwrap();
    let rest = rest.unwrap();
    assert_eq!(prefix.head, all.head);
    assert_eq!(rest.head, all.head);
    assert!(prefix.has_more);
    assert_eq!(rest.after, prefix.scan);
    assert_eq!(rest.scan, all.scan);
    assert_eq!(prefix.scan.seq, prefix.items.last().unwrap().seq.cursor());
    assert_eq!(
        prefix.scan.event_id,
        Some(prefix.items.last().unwrap().event_id)
    );
    let mut joined = prefix.items;
    joined.extend(rest.items);
    assert_eq!(joined, all.items);
    assert!(all.clone().split_prefix(0).is_err());
    let (whole, remainder) = all.clone().split_prefix(all.items.len()).unwrap();
    assert_eq!(whole, all);
    assert!(remainder.is_none());
}

#[tokio::test]
async fn new_filters_validate_thread_channel_snapshot_and_limits_without_writing() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let early = f.app.read(None, requests::ViewerHead).await.unwrap();
    let root = id(&f.post(&a, "root").await.result, "message_id");
    let response = f
        .run(
            &a,
            Mutation::Reply {
                root_message_id: root,
                body: "reply".into(),
                read_message_ids: Vec::new(),
            },
        )
        .await;
    let reply = id(&response.result, "message_id");
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "other".into(),
        },
    )
    .await;
    let before = f.counts().await;
    for (channel, root_message_id, snapshot) in [
        ("dev", reply, None),
        ("other", root, None),
        ("dev", root, Some(early)),
    ] {
        let error = f
            .app
            .read(
                None,
                requests::PinnedRoot {
                    channel: channel.into(),
                    root_message_id,
                    snapshot,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidThread);
    }
    assert_eq!(
        f.app
            .read(
                None,
                requests::ReplyWindow {
                    channel: "dev".into(),
                    root_message_id: reply,
                    window: Default::default()
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidThread
    );
    assert_eq!(
        f.app
            .read(
                None,
                requests::RootWindow {
                    channel: "dev".into(),
                    window: WindowRequest {
                        limit: Some(0),
                        ..Default::default()
                    }
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        f.app
            .read(
                None,
                requests::ViewerChanges {
                    changes: ChangeRequest {
                        after: Checkpoint::start(f.app.project_id()),
                        through: None,
                        limit: Some(201)
                    }
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn observer_reads_preserve_receipts_cancelled_backlog_sessions_and_active_lease_contents() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "author").await;
    let bob = f.join(&b, "reader").await;
    f.post(&a, "before departure").await;
    f.run(&b, Mutation::Leave).await;
    f.post(&a, "inactive interval").await;
    f.run(
        &a,
        Mutation::Reserve {
            patterns: vec!["source/**".into()],
            ttl_seconds: Some(600),
            reason: Some("held".into()),
        },
    )
    .await;
    let mut snapshot = f.store.snapshot().await.unwrap();
    let sessions = (
        snapshot.session(alice).await.unwrap(),
        snapshot.session(bob).await.unwrap(),
    );
    let reservations = snapshot
        .reservations(None, true, f.clock.now(), Default::default(), 201)
        .await
        .unwrap();
    let statistics = snapshot.statistics().await.unwrap();
    drop(snapshot);
    let mut deliveries = delivery_audit(&f).await;
    assert!(deliveries.iter().any(|row| row.1 == "failed_left"));
    deliveries.sort();
    let before = f.counts().await;
    let window = f
        .app
        .read(
            None,
            requests::RootWindow {
                channel: "dev".into(),
                window: Default::default(),
            },
        )
        .await
        .unwrap();
    f.app
        .read(
            None,
            requests::PinnedRoot {
                channel: "dev".into(),
                root_message_id: window.items[0].message.message.id,
                snapshot: Some(window.snapshot),
            },
        )
        .await
        .unwrap();
    f.app
        .read(
            None,
            requests::ViewerChanges {
                changes: ChangeRequest {
                    after: Checkpoint::start(f.app.project_id()),
                    through: None,
                    limit: None,
                },
            },
        )
        .await
        .unwrap();
    let mut snapshot = f.store.snapshot().await.unwrap();
    assert_eq!(
        (
            snapshot.session(alice).await.unwrap(),
            snapshot.session(bob).await.unwrap()
        ),
        sessions
    );
    assert_eq!(
        snapshot
            .reservations(None, true, f.clock.now(), Default::default(), 201)
            .await
            .unwrap(),
        reservations
    );
    assert_eq!(snapshot.statistics().await.unwrap(), statistics);
    drop(snapshot);
    let mut after = delivery_audit(&f).await;
    after.sort();
    assert_eq!(after, deliveries);
    assert_eq!(f.counts().await, before);
}

async fn delivery_audit(f: &Fixture) -> Vec<(Uuid, String, i32, Option<String>)> {
    f.rows("deliveries")
        .await
        .into_iter()
        .map(|row| {
            let id: String = row.try_get("", "id").unwrap();
            (
                Uuid::parse_str(&id).unwrap(),
                row.try_get("", "state").unwrap(),
                row.try_get("", "attempts").unwrap(),
                row.try_get("", "acknowledged_at").unwrap(),
            )
        })
        .collect()
}
