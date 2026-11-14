//! Real read-only message/search/reader retrieval without acknowledgement effects.

mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    read::{PageRequest, Query},
    sequence::Cursor,
};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

fn page() -> PageRequest {
    PageRequest::default()
}
fn history(channel: Option<&str>, thread: Option<Uuid>) -> Query {
    Query::History {
        channel: channel.map(str::to_owned),
        thread,
        page: page(),
    }
}

#[tokio::test]
async fn complete_messages_literal_search_and_thread_filters_do_not_mark_read() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let body = "a%_\\[z] 日本語\0Tail";
    let root = f.post(&a, body).await;
    let root_id = id(&root.result, "message_id");
    f.post(&a, "plain text tail").await;
    let reply = f
        .run(
            &b,
            Mutation::Reply {
                read_message_ids: Vec::new(),
                root_message_id: root_id,
                body: "a direct reply".into(),
            },
        )
        .await;
    let before = f.counts().await;
    for query in ["%", "_", "\\", "[z]", "日本語", "\0Tail", "Tail"] {
        let result = f
            .query(
                None,
                Query::Search {
                    query: query.into(),
                    channel: None,
                    thread: None,
                    page: page(),
                },
            )
            .await;
        assert_eq!(result["items"].as_array().unwrap().len(), 1, "{query:?}");
        assert_eq!(result["items"][0]["id"], root_id.to_string());
        assert_eq!(result["items"][0]["body"], body);
    }
    let case = f
        .query(
            None,
            Query::Search {
                query: "TAIL".into(),
                channel: None,
                thread: None,
                page: page(),
            },
        )
        .await;
    assert_eq!(case["items"], json!([]));
    let empty = f
        .query(
            None,
            Query::Search {
                query: String::new(),
                channel: None,
                thread: None,
                page: page(),
            },
        )
        .await;
    assert_eq!(empty["items"].as_array().unwrap().len(), 3);
    let threaded = f.query(Some(&b), history(Some("DEV"), Some(root_id))).await;
    assert_eq!(threaded["items"].as_array().unwrap().len(), 2);
    assert_eq!(threaded["items"][1]["id"], reply.result["message_id"]);
    let own = f
        .query(
            Some(&a),
            Query::Message {
                message_id: root_id,
                receipts: page(),
            },
        )
        .await;
    assert!(own["message"].get("unread").is_none());
    let detail = f
        .query(
            Some(&b),
            Query::Message {
                message_id: root_id,
                receipts: page(),
            },
        )
        .await;
    assert_eq!(detail["message"]["id"], root_id.to_string());
    assert_eq!(detail["message"]["body"], body);
    assert_eq!(detail["message"]["author"]["actor_id"], alice.to_string());
    assert_eq!(detail["message"]["author"]["handle"], "alice");
    assert_eq!(detail["message"]["channel"], "dev");
    assert_eq!(detail["message"]["thread_id"], root_id.to_string());
    assert!(detail["message"].get("unread").is_none());
    assert!(detail["message"].get("read_at").is_none());
    assert_eq!(detail["receipts"]["items"].as_array().unwrap().len(), 1);
    assert!(
        detail["receipts"]["items"][0]["reads"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn invalid_filters_and_page_inputs_fail_without_any_mutation() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    let root = f.post(&a, "root").await;
    let root_id = id(&root.result, "message_id");
    let reply = f
        .run(
            &a,
            Mutation::Reply {
                read_message_ids: Vec::new(),
                root_message_id: root_id,
                body: "reply".into(),
            },
        )
        .await;
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "other".into(),
        },
    )
    .await;
    let before = f.counts().await;
    for query in [
        history(Some("other"), Some(root_id)),
        history(None, Some(id(&reply.result, "message_id"))),
        history(None, Some(Uuid::new_v4())),
    ] {
        assert_eq!(
            f.app
                .query(None, query)
                .await
                .map(|value| serde_json::to_value(value).unwrap())
                .unwrap_err()
                .code,
            ErrorCode::InvalidThread
        );
    }
    for query in [
        history(Some("missing"), None),
        Query::Message {
            message_id: Uuid::new_v4(),
            receipts: page(),
        },
    ] {
        assert_eq!(
            f.app
                .query(None, query)
                .await
                .map(|value| serde_json::to_value(value).unwrap())
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
    for limit in [0, 201, usize::MAX] {
        assert_eq!(
            f.app
                .query(
                    None,
                    Query::History {
                        channel: None,
                        thread: None,
                        page: PageRequest {
                            limit: Some(limit),
                            after: Cursor::START
                        }
                    }
                )
                .await
                .map(|value| serde_json::to_value(value).unwrap())
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    assert_eq!(
        f.app
            .query(
                None,
                Query::Search {
                    query: "日".repeat(342),
                    channel: None,
                    thread: None,
                    page: page()
                }
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let mut foreign = a.clone();
    foreign.project_id = Uuid::new_v4();
    assert_eq!(
        f.app
            .query(Some(&foreign), history(None, None))
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn canonical_receipts_page_each_actor_once_and_preserve_kind_times() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let carol = f.join(&c, "carol").await;
    let root = f.post(&a, "read only deliberately").await;
    let root = id(&root.result, "message_id");
    for ctx in [&c, &a, &b] {
        f.run(
            ctx,
            Mutation::Read {
                message_ids: vec![root],
            },
        )
        .await;
    }
    let before = f.counts().await;
    let mut after = Cursor::START;
    let mut readers = Vec::new();
    for _ in 0..3 {
        let result = f
            .query(
                None,
                Query::Message {
                    message_id: root,
                    receipts: PageRequest {
                        limit: Some(1),
                        after,
                    },
                },
            )
            .await;
        assert_eq!(result["receipts"]["items"].as_array().unwrap().len(), 1);
        let reader = &result["receipts"]["items"][0];
        assert_eq!(reader["reads"][0]["at"], "2026-10-15T00:00:00.000Z");
        assert_eq!(reader["reads"][0]["kind"], "explicit");
        readers.push(id(&reader["recipient"], "actor_id"));
        if let Some(cursor) = result["receipts"]["next_cursor"].as_str() {
            after = Cursor::parse(cursor).unwrap();
        } else {
            break;
        }
    }
    // Receipt insertion/timestamps are not the ordering key. A reader's retained
    // first join event is immutable and unique even across participation changes.
    assert_eq!(readers, vec![alice, bob, carol]);
    let read = f
        .query(
            Some(&b),
            Query::Message {
                message_id: root,
                receipts: page(),
            },
        )
        .await;
    assert!(read["message"].get("unread").is_none());
    assert!(read["message"].get("read_at").is_none());
    let row = read["receipts"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["recipient"]["actor_id"] == bob.to_string())
        .unwrap();
    assert_eq!(row["reads"][0]["at"], "2026-10-15T00:00:00.000Z");
    assert_eq!(f.counts().await, before);
}
