//! Canonical bounded receipt pages include unread targets and history-only users.

mod support;

use agentchat_core::{
    application::Mutation,
    model::{ActorKind, DeliveryState, OperationActor, ReadKind},
    read::{MessageDetail, PageRequest, Query, QueryResult},
};
use support::{Fixture, native::Gate};
use uuid::Uuid;

async fn detail(f: &Fixture, id: Uuid, receipts: PageRequest) -> MessageDetail {
    let QueryResult::Message(detail) = f
        .app
        .query(
            None,
            Query::Message {
                message_id: id,
                receipts,
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    detail
}

#[tokio::test]
async fn history_only_user_and_unread_target_have_one_typed_canonical_entry_each() {
    let f = Fixture::new().await;
    let sender = f.context();
    let receiver = f.context();
    f.join(&sender, "sender").await;
    let gate = Gate::new(&f);
    gate.bind(&receiver);
    let app = gate.app(&f);
    app.start_launch(&receiver, Some("receiver")).await.unwrap();
    let user = f
        .app
        .execute_actor(
            Uuid::new_v4(),
            &OperationActor::User,
            Mutation::Join { name: None },
        )
        .await
        .unwrap();
    let user_id = Uuid::parse_str(user.result["actor_id"].as_str().unwrap()).unwrap();
    // User participation precedes this body; consume its notices in the shared FIFO.
    while let Some(notice) = app.prepare_native(&receiver).await.unwrap() {
        assert!(matches!(
            notice,
            agentchat_core::delivery::NativeDispatch::Notice(_)
        ));
        app.finish_native(
            &receiver,
            &notice,
            Ok(agentchat_core::delivery::MessageProgress::Accepted(
                Default::default(),
            )),
        )
        .await
        .unwrap();
    }
    let body = "full body\n日本語\0stored".to_owned();
    let posted = f
        .run(
            &sender,
            Mutation::Post {
                channel: None,
                body: body.clone(),
            },
        )
        .await;
    let id = Uuid::parse_str(posted.result["message_id"].as_str().unwrap()).unwrap();
    f.app
        .execute_actor(
            Uuid::new_v4(),
            &OperationActor::User,
            Mutation::Read {
                message_ids: vec![id],
            },
        )
        .await
        .unwrap();
    let page = detail(&f, id, PageRequest::default()).await;
    assert_eq!(page.message.message.body, body);
    assert_eq!(page.receipts.items.len(), 2);
    let native = page
        .receipts
        .items
        .iter()
        .find(|r| r.recipient.kind == ActorKind::Agent)
        .unwrap();
    assert!(native.reads.is_empty());
    let delivery = native.delivery.as_ref().unwrap();
    assert_eq!(delivery.state, DeliveryState::Pending);
    assert_eq!(delivery.history.queued_at, page.message.message.created_at);
    assert!(delivery.history.processing_started_at.is_none());
    assert!(delivery.history.deadline_at.is_none());
    assert!(delivery.history.delivered_at.is_none());
    let user = page
        .receipts
        .items
        .iter()
        .find(|r| r.recipient.kind == ActorKind::User)
        .unwrap();
    assert_eq!(user.recipient.actor_id, user_id);
    assert!(user.delivery.is_none());
    assert_eq!(user.reads.len(), 1);
    assert_eq!(user.reads[0].kind, ReadKind::Explicit);
    let dispatch = app.prepare_message(&receiver).await.unwrap().unwrap();
    assert_eq!(dispatch.input.delivery_id, delivery.delivery_id);
    assert_eq!(dispatch.input.body, body);
    app.finish_message(&receiver, &dispatch, Ok(Default::default()))
        .await
        .unwrap();
    let page = detail(&f, id, PageRequest::default()).await;
    let delivered = page
        .receipts
        .items
        .iter()
        .find_map(|r| r.delivery.as_ref())
        .unwrap();
    assert_eq!(delivered.state, DeliveryState::Delivered);
    assert!(delivered.history.delivered_at.is_some());
    assert!(delivered.history.timed_out_at.is_none());
    let first_history = delivered.history.clone();
    assert!(
        !app.finish_message(&receiver, &dispatch, Ok(Default::default()))
            .await
            .unwrap()
    );
    assert_eq!(
        detail(&f, id, PageRequest::default())
            .await
            .receipts
            .items
            .iter()
            .find_map(|row| row.delivery.as_ref())
            .unwrap()
            .history,
        first_history
    );
    let json = serde_json::to_value(page).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 2);
    for removed in [
        "readers",
        "read_confirmations",
        "first_read_at",
        "read_at",
        "unread",
    ] {
        assert!(json.get(removed).is_none());
        assert!(json["message"].get(removed).is_none());
    }
    let row = json["receipts"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| !row["delivery"].is_null())
        .unwrap();
    for removed in ["accepted_at", "acknowledged_at", "confirmation"] {
        assert!(row["delivery"].get(removed).is_none());
    }
}

#[tokio::test]
async fn default_and_max_receipt_paging_keep_one_complete_body_and_stable_actor_order() {
    let f = Fixture::new().await;
    let sender = f.context();
    f.join(&sender, "sender").await;
    for n in 0..51 {
        f.join(&f.context(), &format!("receiver{n}")).await;
    }
    let body = "r".repeat(65_536);
    let posted = f
        .run(
            &sender,
            Mutation::Post {
                channel: None,
                body: body.clone(),
            },
        )
        .await;
    let id = Uuid::parse_str(posted.result["message_id"].as_str().unwrap()).unwrap();
    let first = detail(&f, id, PageRequest::default()).await;
    assert_eq!(first.message.message.body, body);
    assert_eq!(first.receipts.items.len(), 50);
    let cursor = first.receipts.next_cursor.unwrap();
    let next = detail(
        &f,
        id,
        PageRequest {
            after: cursor,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(next.receipts.items.len(), 1);
    assert!(next.receipts.next_cursor.is_none());
    assert!(
        !first
            .receipts
            .items
            .iter()
            .any(|r| r.recipient.actor_id == next.receipts.items[0].recipient.actor_id)
    );
    let max = detail(
        &f,
        id,
        PageRequest {
            limit: Some(200),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(max.receipts.items.len(), 51);
    assert!(max.receipts.next_cursor.is_none());
    assert_eq!(
        f.app
            .query(
                None,
                Query::Message {
                    message_id: id,
                    receipts: PageRequest {
                        limit: Some(201),
                        ..Default::default()
                    }
                }
            )
            .await
            .unwrap_err()
            .code,
        agentchat_core::ErrorCode::InvalidArgument
    );
}
