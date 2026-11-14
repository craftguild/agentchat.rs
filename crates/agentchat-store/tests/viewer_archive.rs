//! Derived archive presentation uses captured UTC without persisting lifecycle state.

mod support;

use agentchat_core::{
    application::Mutation,
    read::{ChannelListCondition, PageRequest, Query},
};
use support::{Fixture, id};
use uuid::Uuid;

fn channels(condition: ChannelListCondition) -> Query {
    Query::Channels {
        condition,
        page: PageRequest::default(),
    }
}

#[tokio::test]
async fn inclusive_archive_boundary_ignores_non_conversation_activity_and_reply_reactivates() {
    let f = Fixture::new().await;
    let alice = f.context();
    let bob = f.context();
    f.join(&alice, "alice").await;
    f.join(&bob, "bob").await;
    let root = f.post(&alice, "root").await;
    let root_id = id(&root.result, "message_id");

    f.clock.advance(2_592_000 - 1);
    let mut resumed_bob = bob.clone();
    resumed_bob.context_id = Uuid::new_v4();
    resumed_bob.incarnation = Uuid::new_v4();
    f.app.attach(&resumed_bob).await.unwrap();
    f.run(
        &resumed_bob,
        Mutation::Join {
            name: Some("bob".into()),
        },
    )
    .await;
    f.run(
        &resumed_bob,
        Mutation::Read {
            message_ids: vec![root_id],
        },
    )
    .await;
    let active = f.query(None, channels(ChannelListCondition::Active)).await;
    assert_eq!(active["items"][0]["name"], "dev");
    assert_eq!(active["items"][0]["archived"], false);
    assert_eq!(active["items"][0]["mandatory"], true);
    assert_eq!(
        active["items"][0]["last_conversation_at"],
        "2026-10-15T00:00:00.000Z"
    );
    assert_eq!(active["as_of"], "2026-11-13T23:59:59.000Z");

    f.clock.advance(1);
    assert!(
        f.query(None, channels(ChannelListCondition::Active)).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let archived = f
        .query(None, channels(ChannelListCondition::Archived))
        .await;
    assert_eq!(archived["items"][0]["name"], "dev");
    assert_eq!(archived["items"][0]["archived"], true);
    assert_eq!(archived["items"][0]["archive_at"], archived["as_of"]);

    f.run(
        &resumed_bob,
        Mutation::Reply {
            read_message_ids: Vec::new(),
            root_message_id: root_id,
            body: "reply".into(),
        },
    )
    .await;
    let reactivated = f.query(None, channels(ChannelListCondition::Active)).await;
    assert_eq!(
        reactivated["items"][0]["last_conversation_at"],
        reactivated["as_of"]
    );
    assert_eq!(reactivated["items"][0]["archived"], false);

    f.clock.advance(2_592_000);
    let exact = f
        .query(None, channels(ChannelListCondition::Archived))
        .await;
    assert_eq!(exact["items"][0]["archive_at"], exact["as_of"]);

    // Archive classification is presentation only: observer direct/history
    // reads retain their complete-body rights after the inclusive boundary.
    let history = f
        .query(
            None,
            Query::History {
                channel: Some("dev".into()),
                thread: Some(root_id),
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(history["items"].as_array().unwrap().len(), 2);
    let detail = f
        .query(
            None,
            Query::Message {
                message_id: root_id,
                receipts: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(detail["message"]["body"], "root");
}

#[tokio::test]
async fn empty_channels_use_creation_time_and_defaults_are_metadata_not_archive_exemptions() {
    let f = Fixture::new().await;
    let owner = f.context();
    f.join(&owner, "owner").await;
    f.run(&owner, Mutation::ChannelCreate { name: "ops".into() })
        .await;
    f.clock.advance(10);
    f.run(
        &owner,
        Mutation::ChannelCreate {
            name: "planning".into(),
        },
    )
    .await;
    f.app
        .initialize_with_defaults(Some(vec!["planning".into(), "ops".into()]))
        .await
        .unwrap();

    let all = f.query(None, channels(ChannelListCondition::All)).await;
    let items = all["items"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["dev", "ops", "planning"]
    );
    assert_eq!(
        items
            .iter()
            .map(|row| row["mandatory"].as_bool().unwrap())
            .collect::<Vec<_>>(),
        [false, true, true]
    );
    assert!(items.iter().all(|row| !row["archived"].as_bool().unwrap()));

    f.clock.advance(2_592_000 - 1);
    let archived = f
        .query(None, channels(ChannelListCondition::Archived))
        .await;
    let names = archived["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["dev", "ops"]);
    assert_eq!(
        archived["items"][1]["last_conversation_at"],
        "2026-10-15T00:00:00.000Z"
    );
    assert_eq!(archived["items"][1]["mandatory"], true);
    assert_eq!(
        f.query(None, channels(ChannelListCondition::Active)).await["items"][0]["name"],
        "planning"
    );
}
