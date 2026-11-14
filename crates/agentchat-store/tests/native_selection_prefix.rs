//! A bounded search prefix never skips later work or changes mixed native FIFO.
mod support;
use agentchat_core::{
    delivery::NativeItem,
    model::{
        Delivery, DeliveryHistory, DeliveryState, Dispatch, DispatchKind, DispatchNotice,
        DispatchPayload, EventKind, Message, NoticeKind,
    },
    ports::{Storage, Transaction},
    time::{Clock, Timestamp},
};
use serde_json::json;
use support::Fixture;
use uuid::Uuid;

async fn setup() -> (Fixture, Uuid, Uuid, Uuid) {
    let f = Fixture::new().await;
    let author = f.join(&f.context(), "author").await;
    let receiver = f.join(&f.context(), "receiver").await;
    let mut tx = f.store.begin().await.unwrap();
    let channel = tx.channel("dev").await.unwrap().unwrap().id;
    tx.rollback().await.unwrap();
    (f, author, receiver, channel)
}

async fn publish(
    tx: &mut dyn Transaction,
    actor: Uuid,
    receiver: Uuid,
    channel: Option<Uuid>,
    now: Timestamp,
) -> Dispatch {
    let kind = if channel.is_some() {
        DispatchKind::Message
    } else {
        DispatchKind::Notice
    };
    let header = tx.allocate_dispatch(kind, channel, now).await.unwrap();
    let payload = if let Some(channel_id) = channel {
        DispatchPayload::Message(Message {
            id: header.id,
            channel_id,
            author_id: actor,
            root_message_id: None,
            body: "exact immutable native body".into(),
            created_at: now,
            creation_seq: header.creation_seq,
        })
    } else {
        DispatchPayload::Notice(DispatchNotice {
            dispatch_id: header.id,
            kind: NoticeKind::AgentJoined,
            data: json!({"agent_id":actor,"handle":"author"}),
        })
    };
    let delivery = Delivery {
        id: Uuid::new_v4(),
        dispatch_id: header.id,
        recipient_id: receiver,
        state: DeliveryState::Pending,
        history: DeliveryHistory::queued(now),
        wake_eligible: false,
        wake_batch_id: None,
        attempts: 0,
    };
    tx.publish_dispatch(&header, &payload, &[delivery])
        .await
        .unwrap();
    header
}

#[tokio::test]
async fn dense_prefix_preserves_message_notice_order_and_body_only_stop() {
    let (f, author, receiver, channel) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    let start = publish(tx.as_mut(), receiver, author, Some(channel), f.clock.now()).await;
    let first = publish(tx.as_mut(), author, receiver, Some(channel), f.clock.now()).await;
    let notice = publish(tx.as_mut(), author, receiver, None, f.clock.now()).await;
    let next = publish(tx.as_mut(), author, receiver, Some(channel), f.clock.now()).await;
    for _ in 0..100 {
        publish(tx.as_mut(), author, receiver, Some(channel), f.clock.now()).await;
    }
    tx.commit().await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    let item = tx
        .next_native(receiver, start.creation_seq)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&item, NativeItem::Message(_)));
    assert_eq!(item.dispatch().id, first.id);
    let item = tx
        .next_native(receiver, first.creation_seq)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&item, NativeItem::Notice(_)));
    assert_eq!(item.dispatch().id, notice.id);
    assert!(
        tx.next_message(receiver, first.creation_seq)
            .await
            .unwrap()
            .is_none()
    );
    let item = tx
        .next_native(receiver, notice.creation_seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.dispatch().id, next.id);
    assert_eq!(item.delivery().recipient_id, receiver);
    tx.rollback().await.unwrap();
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn unrelated_header_prefix_falls_back_without_advancing_the_original_cursor() {
    let (f, author, receiver, channel) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    let start = publish(tx.as_mut(), receiver, author, Some(channel), f.clock.now()).await;
    for _ in 0..80 {
        publish(tx.as_mut(), receiver, author, Some(channel), f.clock.now()).await;
    }
    let notice = publish(tx.as_mut(), author, receiver, None, f.clock.now()).await;
    let last = publish(tx.as_mut(), author, receiver, Some(channel), f.clock.now()).await;
    tx.commit().await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    let item = tx
        .next_native(receiver, start.creation_seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.dispatch().id, notice.id);
    assert!(
        tx.next_message(receiver, start.creation_seq)
            .await
            .unwrap()
            .is_none()
    );
    let item = tx
        .next_native(receiver, notice.creation_seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.dispatch().id, last.id);
    assert!(
        tx.next_native(receiver, last.creation_seq)
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn prefix_uses_the_current_forward_membership_boundary() {
    let (f, author, receiver, channel) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    let start = publish(tx.as_mut(), receiver, author, Some(channel), f.clock.now()).await;
    let prior_membership =
        publish(tx.as_mut(), author, receiver, Some(channel), f.clock.now()).await;
    // Retain an old pending row while changing the membership boundary. Native
    // selection must independently exclude it, rather than trust the row alone.
    let mut joined = tx
        .allocate_event(EventKind::ChannelJoined, f.clock.now())
        .await
        .unwrap();
    joined.actor_id = Some(receiver);
    joined.channel_id = Some(channel);
    joined.data = json!({"agent_id":receiver,"channel_id":channel});
    tx.publish_event(&joined).await.unwrap();
    tx.remove_member(channel, receiver).await.unwrap();
    tx.add_member(channel, receiver, joined.seq).await.unwrap();
    let current = publish(tx.as_mut(), author, receiver, Some(channel), f.clock.now()).await;
    tx.commit().await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    let item = tx
        .next_native(receiver, start.creation_seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.dispatch().id, current.id);
    assert_ne!(item.dispatch().id, prior_membership.id);
    tx.rollback().await.unwrap();
    f.store.integrity_check().await.unwrap();
}
