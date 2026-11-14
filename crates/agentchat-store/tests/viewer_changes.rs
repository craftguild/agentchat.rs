//! Durable complete change replay, bounded scans and visible history invalidation.
mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    model::EventKind,
    ports::Storage,
    read::{ChangeRequest, Checkpoint, MAX_PAGE_BYTES, ViewerChangeKind, WindowRequest, requests},
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query},
};
use std::collections::BTreeSet;
use support::{Fixture, id};
use uuid::Uuid;

async fn snapshot(f: &Fixture) -> Checkpoint {
    f.app
        .read(
            None,
            requests::RootWindow {
                channel: "dev".into(),
                window: WindowRequest::default(),
            },
        )
        .await
        .unwrap()
        .snapshot
}

#[tokio::test]
async fn bursts_channels_and_old_root_replies_replay_without_skips_across_write_overlap() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let old = id(&f.post(&a, "old root").await.result, "message_id");
    let initial = snapshot(&f).await;
    for n in 0..65 {
        f.post(&a, &format!("new root {n}")).await;
    }
    for n in 0..2 {
        f.run(
            &a,
            Mutation::Reply {
                root_message_id: old,
                body: format!("old reply {n}"),
                read_message_ids: Vec::new(),
            },
        )
        .await;
    }
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "later".into(),
        },
    )
    .await;
    let mut input = ChangeRequest {
        after: initial,
        through: None,
        limit: Some(10),
    };
    let mut seen = BTreeSet::new();
    let mut total = 0;
    let mut replies = 0;
    let mut channels = 0;
    let mut fixed = None;
    let mut expected_counts = f.counts().await;
    loop {
        let batch = f
            .app
            .read(None, requests::ViewerChanges { changes: input })
            .await
            .unwrap();
        assert_eq!(batch.after, input.after);
        assert!(batch.items.len() <= 10);
        assert!(serde_json::to_vec(&batch).unwrap().len() <= MAX_PAGE_BYTES);
        assert!(batch.scan.seq > input.after.seq);
        if let Some(head) = fixed {
            assert_eq!(batch.head, head);
        } else {
            fixed = Some(batch.head);
        }
        for change in batch.items {
            assert!(seen.insert(change.event_id));
            assert!(change.seq.cursor() > batch.after.seq && change.seq.cursor() <= batch.scan.seq);
            match change.change {
                ViewerChangeKind::ChannelCreated { channel } => {
                    assert_eq!(channel.name, "later");
                    channels += 1;
                }
                ViewerChangeKind::MessageCreated { message } => {
                    if message.message.thread_id == old {
                        assert_eq!(
                            message.reply_count, 2,
                            "counts share the captured head even across item continuations"
                        );
                        replies += 1;
                    }
                }
            }
            total += 1;
        }
        assert_eq!(
            f.counts().await,
            expected_counts,
            "read/observation cannot change any retained table count"
        );
        if input.through.is_none() {
            f.post(&a, "after captured head").await;
            expected_counts = f.counts().await;
        }
        input.after = batch.scan;
        input.through = Some(batch.head);
        if !batch.has_more {
            assert_eq!(batch.scan, batch.head);
            break;
        }
    }
    assert_eq!((total, replies, channels), (68, 2, 1));
    let next = f
        .app
        .read(
            None,
            requests::ViewerChanges {
                changes: ChangeRequest {
                    after: input.after,
                    through: None,
                    limit: None,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(next.items.len(), 1);
    let ViewerChangeKind::MessageCreated { message } = &next.items[0].change else {
        panic!("new root")
    };
    assert_eq!(message.message.message.body, "after captured head");
    assert!(!next.has_more);
}

#[tokio::test]
async fn byte_limited_change_batches_retain_every_complete_maximum_body() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let initial = snapshot(&f).await;
    let body = "\0".repeat(65_536);
    let mut expected = Vec::new();
    for _ in 0..5 {
        expected.push(id(&f.post(&a, &body).await.result, "message_id"));
    }
    let before = f.counts().await;
    let mut input = ChangeRequest {
        after: initial,
        through: None,
        limit: Some(200),
    };
    let mut actual = Vec::new();
    loop {
        let batch = f
            .app
            .read(None, requests::ViewerChanges { changes: input })
            .await
            .unwrap();
        assert!(serde_json::to_vec(&batch).unwrap().len() <= MAX_PAGE_BYTES);
        assert_eq!(batch.items.len(), 1);
        let ViewerChangeKind::MessageCreated { message } = &batch.items[0].change else {
            panic!("complete body")
        };
        assert_eq!(message.message.message.body, body);
        actual.push(message.message.message.id);
        input.after = batch.scan;
        input.through = Some(batch.head);
        if !batch.has_more {
            break;
        }
    }
    assert_eq!(actual, expected);
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn irrelevant_spans_have_bounded_scanned_continuations_and_deleted_gaps_reset() {
    let f = Fixture::new().await;
    let actor = f.join(&f.context(), "author").await;
    let initial = snapshot(&f).await;
    let mut tx = f.store.begin().await.unwrap();
    for _ in 0..220 {
        let mut event = tx
            .allocate_event(EventKind::AgentLaunchRetired, f.clock.now())
            .await
            .unwrap();
        event.actor_id = Some(actor);
        event.data = serde_json::json!({"agent_id":actor,"handle":"author"});
        tx.publish_event(&event).await.unwrap();
    }
    tx.commit().await.unwrap();
    let before = f.counts().await;
    let first = f
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
    assert!(first.items.is_empty());
    assert!(first.has_more);
    assert_eq!(first.scan.seq.get() - initial.seq.get(), 200);
    let last = f
        .app
        .read(
            None,
            requests::ViewerChanges {
                changes: ChangeRequest {
                    after: first.scan,
                    through: Some(first.head),
                    limit: None,
                },
            },
        )
        .await
        .unwrap();
    assert!(last.items.is_empty());
    assert!(!last.has_more);
    assert_eq!(last.scan, first.head);
    assert_eq!(f.counts().await, before);
    // Deliberately model a damaged/restored interval using the required ORM
    // builder boundary. It must not look like safely scanned irrelevant data.
    f.inspect
        .execute(
            &Query::delete()
                .from_table(Alias::new("events"))
                .and_where(Expr::col(Alias::new("seq")).eq(initial.seq.get() + 1))
                .to_owned(),
        )
        .await
        .unwrap();
    let error = f
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
        .unwrap_err();
    assert_eq!(error.details["reset_required"], true);
}

#[tokio::test]
async fn replaced_same_sequence_history_wrong_projects_and_invalid_threads_are_refused() {
    let f = Fixture::new().await;
    let initial = snapshot(&f).await;
    let replacement = Uuid::new_v4();
    f.inspect
        .execute(
            &Query::update()
                .table(Alias::new("events"))
                .value(Alias::new("event_id"), replacement.to_string())
                .and_where(Expr::col(Alias::new("seq")).eq(initial.seq.get()))
                .to_owned(),
        )
        .await
        .unwrap();
    let before = f.counts().await;
    let error = f
        .app
        .read(
            None,
            requests::RootWindow {
                channel: "dev".into(),
                window: WindowRequest {
                    snapshot: Some(initial),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["reset_required"], true);
    let fresh = snapshot(&f).await;
    assert_eq!(fresh.seq, initial.seq);
    assert_eq!(fresh.event_id, Some(replacement));
    let wrong = Checkpoint {
        project_id: Uuid::new_v4(),
        ..fresh
    };
    assert_eq!(
        f.app
            .read(
                None,
                requests::ViewerChanges {
                    changes: ChangeRequest {
                        after: wrong,
                        through: None,
                        limit: None
                    }
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(
        f.app
            .read(
                None,
                requests::PinnedRoot {
                    channel: "dev".into(),
                    root_message_id: Uuid::new_v4(),
                    snapshot: Some(fresh)
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidThread
    );
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn inactive_recipient_history_stays_visible_without_rejoining_or_restoring_delivery() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "author").await;
    f.join(&b, "reader").await;
    f.post(&a, "before leave").await;
    f.run(&b, Mutation::Leave).await;
    f.post(&a, "inactive interval").await;
    let before = f.counts().await;
    let page = f
        .app
        .read(
            None,
            requests::RootWindow {
                channel: "dev".into(),
                window: WindowRequest::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|m| m.message.message.body.as_str())
            .collect::<Vec<_>>(),
        ["before leave", "inactive interval"]
    );
    assert!(page.items.iter().all(|m| {
        let wire = serde_json::to_value(&m.message).unwrap();
        wire.get("read_at").is_none() && wire.get("unread").is_none()
    }));
    assert_eq!(f.counts().await, before);
}
