//! Finite page counts/bytes and filtered scan progress against actual SQLite.

mod support;

use agentchat_core::{
    application::Mutation,
    ports::Storage,
    read::{MAX_PAGE_BYTES, PageRequest, Query},
    sequence::Cursor,
};
use serde_json::Value;
use support::{Fixture, id};

fn request(limit: usize, after: Cursor) -> PageRequest {
    PageRequest {
        limit: Some(limit),
        after,
    }
}
fn next(value: &Value) -> Option<Cursor> {
    value["next_cursor"]
        .as_str()
        .map(|v| Cursor::parse(v).unwrap())
}

#[tokio::test]
async fn byte_budget_splits_fully_escaped_bodies_without_skipping_any_message() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let body = "\u{0001}".repeat(65_536);
    let mut expected = Vec::new();
    for _ in 0..3 {
        expected.push(id(&f.post(&a, &body).await.result, "message_id"));
    }
    let before = f.counts().await;
    let mut after = Cursor::START;
    let mut actual = Vec::new();
    let mut pages = 0;
    loop {
        let result = f
            .query(
                None,
                Query::History {
                    channel: None,
                    thread: None,
                    page: request(200, after),
                },
            )
            .await;
        assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_PAGE_BYTES);
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["body"], body);
        actual.push(id(&result["items"][0], "id"));
        pages += 1;
        if let Some(cursor) = next(&result) {
            assert!(cursor > after);
            after = cursor;
        } else {
            break;
        }
        assert!(pages < 4);
    }
    assert_eq!(actual, expected);
    let mut after = Cursor::START;
    let mut inbox_messages = Vec::new();
    for _ in 0..5 {
        let result = f
            .query(
                Some(&b),
                Query::Inbox {
                    page: request(200, after),
                },
            )
            .await;
        assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_PAGE_BYTES);
        for row in result["items"].as_array().unwrap() {
            if !row["message"].is_null() {
                assert_eq!(row["message"]["body"], body);
                inbox_messages.push(id(&row["message"], "id"));
            }
        }
        if let Some(cursor) = next(&result) {
            assert!(cursor > after);
            after = cursor;
        } else {
            break;
        }
    }
    assert_eq!(inbox_messages, expected);
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn item_limit_and_filtered_scan_cursors_cover_history_without_rescanning_gaps() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    let mut expected = Vec::new();
    for i in 0..205 {
        expected.push(id(
            &f.post(&a, &format!("item {i:03}")).await.result,
            "message_id",
        ));
    }
    let first = f
        .query(
            None,
            Query::History {
                channel: None,
                thread: None,
                page: request(200, Cursor::START),
            },
        )
        .await;
    assert_eq!(first["items"].as_array().unwrap().len(), 200);
    let cursor = next(&first).unwrap();
    let second = f
        .query(
            None,
            Query::History {
                channel: None,
                thread: None,
                page: request(200, cursor),
            },
        )
        .await;
    assert_eq!(second["items"].as_array().unwrap().len(), 5);
    assert!(second["next_cursor"].is_null());
    let actual = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["items"].as_array().unwrap())
        .map(|row| id(row, "id"))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    let empty = f
        .query(
            None,
            Query::Search {
                query: "absent literal".into(),
                channel: Some("dev".into()),
                thread: None,
                page: request(1, Cursor::START),
            },
        )
        .await;
    assert_eq!(empty["scan_cursor"], empty["head"]);
    assert!(empty["next_cursor"].is_null());
    let head = Cursor::parse(empty["scan_cursor"].as_str().unwrap()).unwrap();
    let new = f.post(&a, "absent literal arrives later").await;
    let caught = f
        .query(
            None,
            Query::Search {
                query: "absent literal".into(),
                channel: Some("dev".into()),
                thread: None,
                page: request(1, head),
            },
        )
        .await;
    assert_eq!(caught["items"].as_array().unwrap().len(), 1);
    assert_eq!(caught["items"][0]["id"], new.result["message_id"]);
    let future = Cursor::new(i64::MAX).unwrap();
    let empty = f
        .query(
            None,
            Query::Events {
                channel: None,
                page: request(1, future),
            },
        )
        .await;
    assert_eq!(empty["scan_cursor"], i64::MAX.to_string());
    assert_eq!(empty["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn filtered_events_skip_passive_intervals_and_resume_after_the_reported_scan_head() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "quiet".into(),
        },
    )
    .await;
    f.run(
        &a,
        Mutation::ChannelJoin {
            name: "quiet".into(),
        },
    )
    .await;
    let channel = f
        .query(
            None,
            Query::Events {
                channel: Some("quiet".into()),
                page: request(200, Cursor::START),
            },
        )
        .await;
    let cursor = Cursor::parse(channel["scan_cursor"].as_str().unwrap()).unwrap();
    f.post(&a, "outside this channel").await;
    let quiet = f
        .query(
            None,
            Query::Events {
                channel: Some("quiet".into()),
                page: request(1, cursor),
            },
        )
        .await;
    assert!(quiet["items"].as_array().unwrap().is_empty());
    assert_eq!(quiet["scan_cursor"], quiet["head"]);
    let after = Cursor::parse(quiet["scan_cursor"].as_str().unwrap()).unwrap();
    let post = f
        .run(
            &a,
            Mutation::Post {
                channel: Some("quiet".into()),
                body: "now visible".into(),
            },
        )
        .await;
    let resumed = f
        .query(
            None,
            Query::Events {
                channel: Some("quiet".into()),
                page: request(1, after),
            },
        )
        .await;
    assert_eq!(resumed["items"][0]["message_id"], post.result["message_id"]);
    assert_eq!(resumed["items"][0]["seq"], post.result["seq"]);
    assert_eq!(
        f.app.event_head().await.unwrap(),
        Cursor::parse(resumed["head"].as_str().unwrap()).unwrap()
    );
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn snapshot_remains_consistent_while_the_same_store_commits_a_later_message() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    let mut old = f.store.snapshot().await.unwrap();
    let head = old.head();
    let post = f
        .post(&a, "writer can commit beside a read-only snapshot")
        .await;
    assert_eq!(old.statistics().await.unwrap().messages, 0);
    assert!(
        old.messages(&Default::default(), Cursor::START, 201)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(old.head(), head);
    let events = old.events(Cursor::START, 201, None).await.unwrap();
    assert!(events.iter().all(|e| e.seq.cursor() <= head));
    drop(old);
    let current = f
        .query(
            None,
            Query::History {
                channel: None,
                thread: None,
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(current["items"][0]["id"], post.result["message_id"]);
    assert!(Cursor::parse(current["head"].as_str().unwrap()).unwrap() > head);
}
