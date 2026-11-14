//! Actual SQLite backward windows, immutable snapshot watermarks and full bodies.
mod support;

use agentchat_core::{
    application::Mutation,
    model::Context,
    ports::Storage,
    read::{MAX_PAGE_BYTES, MessageWindow, WindowRequest, requests},
};
use support::{Fixture, id};
use uuid::Uuid;

async fn roots(f: &Fixture, window: WindowRequest) -> MessageWindow {
    f.app
        .read(
            None,
            requests::RootWindow {
                channel: "dev".into(),
                window,
            },
        )
        .await
        .unwrap()
}
async fn reply(f: &Fixture, context: &Context, root: Uuid, body: &str) {
    f.run(
        context,
        Mutation::Reply {
            root_message_id: root,
            body: body.into(),
            read_message_ids: Vec::new(),
        },
    )
    .await;
}

#[tokio::test]
async fn root_and_reply_windows_keep_independent_fifty_record_targets_and_snapshot_counts() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let mut ids = Vec::new();
    for n in 0..120 {
        ids.push(id(
            &f.post(&a, &format!("root {n}")).await.result,
            "message_id",
        ));
    }
    for n in 0..75 {
        reply(&f, &a, ids[0], &format!("reply {n}")).await;
    }
    let before = f.counts().await;
    let latest = roots(&f, WindowRequest::default()).await;
    assert_eq!(latest.items.len(), 50);
    assert_eq!(
        latest
            .items
            .iter()
            .map(|m| m.message.message.id)
            .collect::<Vec<_>>(),
        ids[70..]
    );
    let pinned = f
        .app
        .read(
            None,
            requests::PinnedRoot {
                channel: "dev".into(),
                root_message_id: ids[0],
                snapshot: Some(latest.snapshot),
            },
        )
        .await
        .unwrap();
    assert_eq!(pinned.root.reply_count, 75);
    let replies = f
        .app
        .read(
            None,
            requests::ReplyWindow {
                channel: "dev".into(),
                root_message_id: ids[0],
                window: WindowRequest {
                    snapshot: Some(latest.snapshot),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(replies.items.len(), 50);
    assert_eq!(replies.items[0].message.message.body, "reply 25");
    assert_eq!(replies.items[49].message.message.body, "reply 74");
    assert!(
        replies
            .items
            .iter()
            .all(|m| m.message.message.root_message_id == Some(ids[0]) && m.reply_count == 75)
    );
    assert_eq!(f.counts().await, before);

    // Later writes cannot enter the older pages or their count aggregates.
    f.post(&a, "later root").await;
    reply(&f, &a, ids[0], "later reply").await;
    let after_writes = f.counts().await;
    let older = roots(
        &f,
        WindowRequest {
            snapshot: Some(latest.snapshot),
            before: latest.next_before,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(older.items.len(), 50);
    assert_eq!(
        older
            .items
            .iter()
            .map(|m| m.message.message.id)
            .collect::<Vec<_>>(),
        ids[20..70]
    );
    let oldest = roots(
        &f,
        WindowRequest {
            snapshot: Some(latest.snapshot),
            before: older.next_before,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(oldest.items.len(), 20);
    assert_eq!(oldest.items[0].message.message.id, ids[0]);
    assert_eq!(oldest.items[0].reply_count, 75);
    assert!(oldest.next_before.is_none());
    let older_replies = f
        .app
        .read(
            None,
            requests::ReplyWindow {
                channel: "dev".into(),
                root_message_id: ids[0],
                window: WindowRequest {
                    snapshot: Some(latest.snapshot),
                    before: replies.next_before,
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(older_replies.items.len(), 25);
    assert_eq!(older_replies.items[0].message.message.body, "reply 0");
    assert_eq!(older_replies.items[24].message.message.body, "reply 24");
    assert!(older_replies.next_before.is_none());
    let fresh = f
        .app
        .read(
            None,
            requests::PinnedRoot {
                channel: "dev".into(),
                root_message_id: ids[0],
                snapshot: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(fresh.root.reply_count, 76);
    assert_eq!(f.counts().await, after_writes);
}

#[tokio::test]
async fn maximum_expanding_bodies_page_completely_and_pinned_root_has_its_own_budget() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let body = "\0".repeat(65_536);
    let mut ids = Vec::new();
    for _ in 0..6 {
        ids.push(id(&f.post(&a, &body).await.result, "message_id"));
    }
    reply(&f, &a, ids[0], &body).await;
    let before = f.counts().await;
    let mut input = WindowRequest::default();
    let mut all = Vec::new();
    loop {
        let page = roots(&f, input).await;
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_PAGE_BYTES);
        assert_eq!(
            page.items.len(),
            1,
            "bytes shorten the display target without shortening bodies"
        );
        assert_eq!(page.items[0].message.message.body, body);
        all.push(page.items[0].message.message.id);
        input.snapshot = Some(page.snapshot);
        let Some(before) = page.next_before else {
            break;
        };
        input.before = Some(before);
    }
    all.reverse();
    assert_eq!(all, ids);
    let pinned = f
        .app
        .read(
            None,
            requests::PinnedRoot {
                channel: "dev".into(),
                root_message_id: ids[0],
                snapshot: input.snapshot,
            },
        )
        .await
        .unwrap();
    let replies = f
        .app
        .read(
            None,
            requests::ReplyWindow {
                channel: "dev".into(),
                root_message_id: ids[0],
                window: WindowRequest {
                    snapshot: input.snapshot,
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(pinned.root.message.message.body, body);
    assert_eq!(replies.items[0].message.message.body, body);
    assert!(serde_json::to_vec(&pinned).unwrap().len() <= MAX_PAGE_BYTES);
    assert!(serde_json::to_vec(&replies).unwrap().len() <= MAX_PAGE_BYTES);
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn related_labels_and_counts_use_the_same_wal_snapshot_while_writer_progresses() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let root = id(&f.post(&a, "original").await.result, "message_id");
    let initial = roots(&f, WindowRequest::default()).await;
    let mut snapshot = f.store.snapshot().await.unwrap();
    let channel = snapshot.channel("dev").await.unwrap().unwrap();
    reply(&f, &a, root, "concurrent").await;
    let rows = snapshot
        .message_window(channel.id, None, None, snapshot.head(), 51)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reply_count, 0);
    drop(snapshot);
    let old = f
        .app
        .read(
            None,
            requests::PinnedRoot {
                channel: "dev".into(),
                root_message_id: root,
                snapshot: Some(initial.snapshot),
            },
        )
        .await
        .unwrap();
    assert_eq!(old.root.reply_count, 0);
    let fresh = f
        .app
        .read(
            None,
            requests::PinnedRoot {
                channel: "dev".into(),
                root_message_id: root,
                snapshot: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(fresh.root.reply_count, 1);
}
