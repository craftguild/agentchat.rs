//! Actual application/SeaORM atomicity under failed canonical Markdown extraction.
mod support;
use agentchat_core::{ErrorCode, application::Mutation};
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn incomplete_post_or_read_with_reply_has_no_partial_business_effects() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let root = id(&f.post(&a, "@bob root").await.result, "message_id");
    let before = f.counts().await;
    let key = Uuid::new_v4();
    for source in [
        format!("{}@hidden\n", "> ".repeat(65)),
        format!("{}- @hidden\n", "> ".repeat(64)),
    ] {
        for command in [
            Mutation::Post {
                channel: Some("dev".into()),
                body: source.clone(),
            },
            Mutation::Reply {
                root_message_id: root,
                read_message_ids: vec![root],
                body: source,
            },
        ] {
            let error = f.app.execute(key, &b, command).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument);
            assert_eq!(error.details["markdown_parse_status"], "incomplete");
            // Includes the Header, Message subtype, deliveries and history, as
            // well as reads/events/request receipts from the whole savepoint.
            assert_eq!(f.counts().await, before);
            assert_eq!(f.count("read_receipts").await, 0);
        }
    }
    let operation = Mutation::Post {
        channel: Some("dev".into()),
        body: "@alice repaired".into(),
    };
    let result = f.app.execute(key, &b, operation.clone()).await.unwrap();
    let after = f.counts().await;
    let replay = f.app.execute(key, &b, operation).await.unwrap();
    assert_eq!(result.result, replay.result);
    assert!(replay.event_head.is_none());
    assert_eq!(f.counts().await, after);
    f.store.integrity_check().await.unwrap();
}
