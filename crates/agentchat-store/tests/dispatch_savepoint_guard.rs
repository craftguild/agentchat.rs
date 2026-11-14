//! A proof cannot hide preexisting corruption, parent drafts or rollback effects.
mod support;

use agentchat_core::{
    model::{
        Dispatch, DispatchKind, DispatchNotice, DispatchPayload, EventKind, Message, NoticeKind,
    },
    ports::{Storage, Transaction},
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query},
};
use serde_json::json;
use support::Fixture;
use uuid::Uuid;

async fn setup() -> (Fixture, Uuid, Uuid) {
    let f = Fixture::new().await;
    let actor = f.join(&f.context(), "author").await;
    let mut tx = f.store.begin().await.unwrap();
    let channel = tx.channel("dev").await.unwrap().unwrap().id;
    tx.rollback().await.unwrap();
    (f, actor, channel)
}

fn notice(header: &Dispatch, actor: Uuid) -> DispatchPayload {
    DispatchPayload::Notice(DispatchNotice {
        dispatch_id: header.id,
        kind: NoticeKind::AgentJoined,
        data: json!({"agent_id":actor,"handle":"author"}),
    })
}

fn message(header: &Dispatch, actor: Uuid, channel: Uuid) -> DispatchPayload {
    DispatchPayload::Message(Message {
        id: header.id,
        channel_id: channel,
        author_id: actor,
        root_message_id: None,
        body: "immutable guard regression".into(),
        created_at: header.created_at,
        creation_seq: header.creation_seq,
    })
}

async fn prove(tx: &mut dyn Transaction) {
    tx.savepoint().await.unwrap().commit().await.unwrap();
}

#[tokio::test]
async fn first_child_and_new_outer_refuse_preexisting_sealed_subtype_corruption() {
    for corruption in ["missing_message", "second_notice"] {
        let (f, actor, channel) = setup().await;
        let mut tx = f.store.begin().await.unwrap();
        let header = tx
            .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
            .await
            .unwrap();
        tx.publish_dispatch(&header, &message(&header, actor, channel), &[])
            .await
            .unwrap();
        prove(tx.as_mut()).await;
        tx.commit().await.unwrap();
        // This separate synthetic connection cannot write while an Immediate
        // transaction is held. Corrupt only after that outer has completed.
        if corruption == "missing_message" {
            f.inspect
                .execute(
                    &Query::delete()
                        .from_table(Alias::new("dispatch_messages"))
                        .and_where(Expr::col(Alias::new("dispatch_id")).eq(header.id.to_string()))
                        .to_owned(),
                )
                .await
                .unwrap();
        } else {
            f.inspect
                .execute(
                    &Query::insert()
                        .into_table(Alias::new("dispatch_notices"))
                        .columns(["dispatch_id", "kind", "data"].map(Alias::new))
                        .values_panic([
                            header.id.to_string().into(),
                            "agent.joined".into(),
                            json!({"agent_id":actor,"handle":"author"}).into(),
                        ])
                        .to_owned(),
                )
                .await
                .unwrap();
        }
        let mut tx = f.store.begin().await.unwrap();
        assert!(
            tx.savepoint().await.unwrap().commit().await.is_err(),
            "{corruption}"
        );
        assert!(tx.commit().await.is_err(), "{corruption}");
    }
}

#[tokio::test]
async fn failed_first_guard_does_not_certify_a_parent_draft() {
    let (f, actor, _) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    assert!(tx.savepoint().await.unwrap().commit().await.is_err());
    assert!(tx.savepoint().await.unwrap().commit().await.is_err());
    tx.publish_dispatch(&header, &notice(&header, actor), &[])
        .await
        .unwrap();
    prove(tx.as_mut()).await;
    tx.commit().await.unwrap();
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn draft_created_after_proof_and_ignored_publish_error_remain_invalid() {
    let (f, actor, channel) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    prove(tx.as_mut()).await;
    let header = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    assert!(
        tx.publish_dispatch(&header, &message(&header, actor, channel), &[])
            .await
            .is_err()
    );
    assert!(tx.savepoint().await.unwrap().commit().await.is_err());
    assert!(tx.commit().await.is_err());
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn ancestor_rollback_restoring_a_published_parent_draft_invalidates_child_commit() {
    let (f, actor, _) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    {
        let mut ancestor = tx.savepoint().await.unwrap();
        let mut child = ancestor.savepoint().await.unwrap();
        child
            .publish_dispatch(&header, &notice(&header, actor), &[])
            .await
            .unwrap();
        child.commit().await.unwrap();
        ancestor.rollback().await.unwrap();
    }
    // The global proof succeeded inside the rolled-back ancestor. Retaining
    // every touched ID must still find this Header's restored draft state.
    assert!(tx.savepoint().await.unwrap().commit().await.is_err());
    assert!(tx.commit().await.is_err());
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn rolled_back_new_header_does_not_create_a_phantom_incomplete_aggregate() {
    let (f, _, _) = setup().await;
    let before = f.count("dispatches").await;
    let mut tx = f.store.begin().await.unwrap();
    prove(tx.as_mut()).await;
    let mut child = tx.savepoint().await.unwrap();
    child
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    child.rollback().await.unwrap();
    prove(tx.as_mut()).await;
    tx.commit().await.unwrap();
    assert_eq!(f.count("dispatches").await, before);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn bounded_tracking_falls_back_without_accepting_drafts_or_rejecting_valid_writes() {
    let (f, actor, _) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    prove(tx.as_mut()).await;
    let mut last = None;
    for i in 0..257 {
        let header = tx
            .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
            .await
            .unwrap();
        if i < 256 {
            tx.publish_dispatch(&header, &notice(&header, actor), &[])
                .await
                .unwrap();
        }
        last = Some(header);
    }
    assert!(tx.savepoint().await.unwrap().commit().await.is_err());
    let last = last.unwrap();
    tx.publish_dispatch(&last, &notice(&last, actor), &[])
        .await
        .unwrap();
    prove(tx.as_mut()).await;
    tx.commit().await.unwrap();
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn dispatch_proof_never_exempts_the_unsealed_event_guard() {
    let (f, _, _) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    prove(tx.as_mut()).await;
    tx.allocate_event(EventKind::AgentJoined, f.clock.now())
        .await
        .unwrap();
    assert!(tx.savepoint().await.unwrap().commit().await.is_err());
    assert!(tx.commit().await.is_err());
    f.store.integrity_check().await.unwrap();
}
