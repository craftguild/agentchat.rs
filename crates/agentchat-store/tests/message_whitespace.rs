//! Message persistence strips boundary whitespace without changing request identity.

mod support;

use agentchat_core::{ErrorCode, application::Mutation, model::OperationActor};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

const WHITESPACE: &str = "\t\n\u{b}\u{c}\r \u{85}\u{a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}";

#[tokio::test]
async fn root_and_reply_store_trimmed_bodies_and_events_for_both_actor_kinds() {
    for human in [false, true] {
        let f = Fixture::new().await;
        let context = f.context();
        f.join(&context, "alice").await;
        let bob = f.join(&f.context(), "bob").await;
        let actor = if human {
            f.app
                .execute_actor(
                    Uuid::new_v4(),
                    &OperationActor::User,
                    Mutation::Join { name: None },
                )
                .await
                .unwrap();
            OperationActor::User
        } else {
            OperationActor::Native(context)
        };
        let body = "@bob 日本語😀\r\n\n  internal indentation \t\0\nlast line";
        let raw = format!("{WHITESPACE}{body}{WHITESPACE}");
        let mut root = None;
        for reply in [false, true] {
            let command = |body: String| {
                if reply {
                    Mutation::Reply {
                        read_message_ids: Vec::new(),
                        root_message_id: root.unwrap(),
                        body,
                    }
                } else {
                    Mutation::Post {
                        channel: Some("dev".into()),
                        body,
                    }
                }
            };
            let key = Uuid::new_v4();
            let posted = f
                .app
                .execute_actor(key, &actor, command(raw.clone()))
                .await
                .unwrap();
            assert_eq!(posted.result["mentioned_agent_ids"], json!([bob]));
            let message_id = id(&posted.result, "message_id");
            let row = f
                .rows("dispatch_messages")
                .await
                .into_iter()
                .find(|r| r.try_get::<String>("", "dispatch_id").unwrap() == message_id.to_string())
                .unwrap();
            assert_eq!(row.try_get::<String>("", "body").unwrap(), body);
            let event = f
                .rows("events")
                .await
                .into_iter()
                .find(|r| {
                    r.try_get::<Option<String>>("", "message_id").unwrap()
                        == Some(message_id.to_string())
                })
                .unwrap();
            let data: serde_json::Value =
                serde_json::from_str(&event.try_get::<String>("", "data").unwrap()).unwrap();
            assert_eq!(data["body_bytes"], body.len());
            assert_eq!(data["preview"], body);
            let before = f.counts().await;
            let retry = f
                .app
                .execute_actor(key, &actor, command(raw.clone()))
                .await
                .unwrap();
            assert_eq!(retry.result, posted.result);
            assert!(retry.event_head.is_none());
            // A request still identifies its original submitted arguments, including
            // padding. This preserves existing receipts without rewriting old data.
            assert_eq!(
                f.app
                    .execute_actor(key, &actor, command(body.into()))
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::IdempotencyConflict
            );
            assert_eq!(f.counts().await, before);
            root = Some(root.unwrap_or(message_id));
        }
        assert_eq!(f.count("dispatch_messages").await, 2);
        assert_eq!(f.count("read_receipts").await, 0);
        f.store.integrity_check().await.unwrap();
    }
}

#[tokio::test]
async fn whitespace_only_and_oversized_raw_bodies_leave_no_message_or_reply_reads() {
    let f = Fixture::new().await;
    let context = f.context();
    f.join(&context, "alice").await;
    let root = id(&f.post(&context, "root").await.result, "message_id");
    for raw in [WHITESPACE.into(), format!(" {} ", "x".repeat(65_535))] {
        for reply in [false, true] {
            let before = f.counts().await;
            let command = if reply {
                Mutation::Reply {
                    read_message_ids: vec![root],
                    root_message_id: root,
                    body: raw.clone(),
                }
            } else {
                Mutation::Post {
                    channel: None,
                    body: raw.clone(),
                }
            };
            assert_eq!(
                f.app
                    .execute(Uuid::new_v4(), &context, command)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidArgument
            );
            assert_eq!(f.counts().await, before);
        }
    }
    let kept = "\0\u{200b}\u{feff}kept\u{feff}\u{200b}\0";
    f.post(&context, &format!(" {kept}\n")).await;
    assert!(
        f.rows("dispatch_messages")
            .await
            .iter()
            .any(|r| r.try_get::<String>("", "body").unwrap() == kept)
    );
    f.store.integrity_check().await.unwrap();
}
