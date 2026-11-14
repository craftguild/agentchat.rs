//! Message-only reads and independent audit/Dispatch ordering remain coherent.
mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    model::{
        Context, Delivery, DeliveryHistory, DeliveryState, Dispatch, DispatchKind, DispatchNotice,
        DispatchPayload, Event, EventKind, NoticeKind,
    },
    ports::{Storage, Transaction},
    read::{ChangeRequest, Checkpoint, PageRequest, ViewerChangeKind, WindowRequest, requests},
    sequence::{Cursor, Sequence},
    time::{Clock, Timestamp},
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query},
};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

struct Views {
    f: Fixture,
    author: Context,
    reader: Context,
    author_id: Uuid,
    reader_id: Uuid,
}

impl Views {
    async fn new() -> Self {
        let f = Fixture::new().await;
        let author = f.context();
        let author_id = f.join(&author, "author").await;
        let reader = f.context();
        let reader_id = f.native_join(&reader, "reader").await;
        Self {
            f,
            author,
            reader,
            author_id,
            reader_id,
        }
    }

    async fn post(&self, body: &str) -> Uuid {
        id(&self.f.post(&self.author, body).await.result, "message_id")
    }

    async fn head(&self) -> Checkpoint {
        self.f.app.read(None, requests::ViewerHead).await.unwrap()
    }

    async fn notice(&self, recipients: &[Uuid]) -> Dispatch {
        let mut tx = self.f.store.begin().await.unwrap();
        let header = notice(tx.as_mut(), self.author_id, recipients, self.f.clock.now()).await;
        tx.commit().await.unwrap();
        header
    }
}

async fn notice(
    tx: &mut dyn Transaction,
    actor: Uuid,
    recipients: &[Uuid],
    now: Timestamp,
) -> Dispatch {
    let header = tx
        .allocate_dispatch(DispatchKind::Notice, None, now)
        .await
        .unwrap();
    let payload = DispatchPayload::Notice(DispatchNotice {
        dispatch_id: header.id,
        kind: NoticeKind::AgentJoined,
        data: json!({"agent_id": actor, "handle": "author"}),
    });
    let deliveries = recipients
        .iter()
        .map(|recipient| Delivery {
            id: Uuid::new_v4(),
            dispatch_id: header.id,
            recipient_id: *recipient,
            state: DeliveryState::Pending,
            wake_eligible: false,
            wake_batch_id: None,
            attempts: 0,
            history: DeliveryHistory::queued(now),
        })
        .collect::<Vec<_>>();
    // This explicit policy action creates the aggregate. No audit replay or
    // event insertion is used as a delivery-producing shortcut.
    tx.publish_dispatch(&header, &payload, &deliveries)
        .await
        .unwrap();
    header
}

async fn audit(tx: &mut dyn Transaction, actor: Uuid, now: Timestamp) -> Event {
    let mut event = tx
        .allocate_event(EventKind::AgentJoined, now)
        .await
        .unwrap();
    event.actor_id = Some(actor);
    event.data = json!({"agent_id": actor, "handle": "author"});
    tx.publish_event(&event).await.unwrap();
    event
}

async fn inventory(f: &Fixture) -> Vec<usize> {
    let mut result = Vec::new();
    for table in [
        "dispatches",
        "dispatch_messages",
        "dispatch_notices",
        "deliveries",
        "delivery_state_transitions",
        "events",
        "read_receipts",
        "idempotency",
        "mentions",
    ] {
        result.push(f.count(table).await);
    }
    result
}

#[tokio::test]
async fn notice_uuid_is_neither_message_read_target_nor_reply_root_and_mixed_writes_roll_back() {
    let v = Views::new().await;
    let message = v.post("retained root").await;
    let control = v.notice(&[v.reader_id]).await;
    let before = inventory(&v.f).await;
    let lookup =
        v.f.app
            .read(
                None,
                requests::Message {
                    message_id: control.id,
                    receipts: PageRequest::default(),
                },
            )
            .await
            .unwrap_err();
    assert_eq!(lookup.code, ErrorCode::NotFound);
    let pinned =
        v.f.app
            .read(
                None,
                requests::PinnedRoot {
                    channel: "dev".into(),
                    root_message_id: control.id,
                    snapshot: None,
                },
            )
            .await
            .unwrap_err();
    assert_eq!(pinned.code, ErrorCode::InvalidThread);

    let rejected = [
        (
            Mutation::Read {
                message_ids: vec![control.id],
            },
            ErrorCode::NotFound,
        ),
        (
            Mutation::Read {
                message_ids: vec![message, control.id],
            },
            ErrorCode::NotFound,
        ),
        // Valid explicit reads happen before reply-root validation inside one
        // mutation; a Notice root must roll those earlier writes back too.
        (
            Mutation::Reply {
                root_message_id: control.id,
                read_message_ids: vec![message],
                body: "must never become a reply".into(),
            },
            ErrorCode::InvalidThread,
        ),
        (
            Mutation::Reply {
                root_message_id: message,
                read_message_ids: vec![message, control.id],
                body: "mixed read must prevent this reply".into(),
            },
            ErrorCode::NotFound,
        ),
    ];
    for (command, expected) in rejected {
        let request = Uuid::new_v4();
        let error =
            v.f.app
                .execute(request, &v.reader, command)
                .await
                .unwrap_err();
        assert_eq!(error.code, expected);
        assert_eq!(inventory(&v.f).await, before);
        let mut tx = v.f.store.begin().await.unwrap();
        assert!(tx.idempotency(request).await.unwrap().is_none());
        assert!(tx.receipt(message, v.reader_id).await.unwrap().is_none());
        assert!(tx.receipt(control.id, v.reader_id).await.unwrap().is_none());
        assert!(tx.message(control.id).await.unwrap().is_none());
        assert_eq!(
            tx.dispatch(control.id).await.unwrap(),
            Some(control.clone())
        );
        tx.rollback().await.unwrap();
    }
    // The same real Message remains a valid independent explicit-read target.
    v.f.run(
        &v.reader,
        Mutation::Read {
            message_ids: vec![message],
        },
    )
    .await;
    assert_eq!(v.f.count("read_receipts").await, before[6] + 1);
    assert_eq!(v.f.count("dispatch_messages").await, before[1]);
    let mut tx = v.f.store.begin().await.unwrap();
    assert!(tx.receipt(message, v.reader_id).await.unwrap().is_some());
    assert!(tx.receipt(control.id, v.reader_id).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn history_search_and_viewer_are_message_only_while_inbox_retains_both_shapes_in_order() {
    let v = Views::new().await;
    let start = v.head().await;
    let first = v.post("first retained needle").await;
    let control = v.notice(&[v.reader_id]).await;
    let second = v.post("second retained needle").await;
    let before = inventory(&v.f).await;

    let history =
        v.f.app
            .read(
                None,
                requests::History {
                    channel: Some("dev".into()),
                    thread: None,
                    page: PageRequest::default(),
                },
            )
            .await
            .unwrap();
    assert_eq!(
        history
            .items
            .iter()
            .map(|item| item.message.id)
            .collect::<Vec<_>>(),
        [first, second]
    );
    let search =
        v.f.app
            .read(
                None,
                requests::Search {
                    query: "needle".into(),
                    channel: None,
                    thread: None,
                    page: PageRequest::default(),
                },
            )
            .await
            .unwrap();
    assert_eq!(
        search
            .items
            .iter()
            .map(|item| item.message.id)
            .collect::<Vec<_>>(),
        [first, second]
    );
    // The Notice contains handle "author"; conversation search must not match
    // that typed JSON content when neither Message body contains the substring.
    let notice_text =
        v.f.app
            .read(
                None,
                requests::Search {
                    query: "author".into(),
                    channel: None,
                    thread: None,
                    page: PageRequest::default(),
                },
            )
            .await
            .unwrap();
    assert!(notice_text.items.is_empty());
    let window =
        v.f.app
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
        window
            .items
            .iter()
            .map(|item| item.message.message.id)
            .collect::<Vec<_>>(),
        [first, second]
    );
    let changes =
        v.f.app
            .read(
                None,
                requests::ViewerChanges {
                    changes: ChangeRequest {
                        after: start,
                        through: None,
                        limit: None,
                    },
                },
            )
            .await
            .unwrap();
    let projected = changes
        .items
        .iter()
        .map(|change| {
            let ViewerChangeKind::MessageCreated { message } = &change.change else {
                panic!("only the two new conversation Messages are Viewer changes")
            };
            message.message.message.id
        })
        .collect::<Vec<_>>();
    assert_eq!(projected, [first, second]);
    assert!(!changes.has_more);
    assert_eq!(changes.scan, changes.head);

    let inbox =
        v.f.app
            .read(
                Some(&v.reader),
                requests::Inbox {
                    page: PageRequest::default(),
                },
            )
            .await
            .unwrap();
    let expected = [first, control.id, second];
    let selected = inbox
        .items
        .iter()
        .filter(|item| expected.contains(&item.dispatch.id))
        .collect::<Vec<_>>();
    assert_eq!(
        selected
            .iter()
            .map(|item| item.dispatch.id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        selected
            .windows(2)
            .all(|pair| pair[0].dispatch.creation_seq < pair[1].dispatch.creation_seq)
    );
    for item in &selected {
        assert_eq!(item.delivery.dispatch_id, item.dispatch.id);
        assert_eq!(item.delivery.state, DeliveryState::Pending);
        let wire = serde_json::to_value(item).unwrap();
        if item.dispatch.id == control.id {
            assert_eq!(item.dispatch.kind, DispatchKind::Notice);
            assert!(item.message.is_none());
            assert_eq!(item.notice.as_ref().unwrap().dispatch_id, control.id);
            assert_eq!(wire["dispatch"]["kind"], "notice");
            assert!(serde_json::Value::is_null(&wire["message"]));
            assert_eq!(wire["notice"]["dispatch_id"], control.id.to_string());
        } else {
            assert_eq!(item.dispatch.kind, DispatchKind::Message);
            assert!(item.notice.is_none());
            assert_eq!(item.message.as_ref().unwrap().message.id, item.dispatch.id);
            assert_eq!(wire["dispatch"]["kind"], "message");
            assert!(serde_json::Value::is_null(&wire["notice"]));
            assert_eq!(wire["message"]["id"], item.dispatch.id.to_string());
        }
        assert!(wire.get("event").is_none());
    }

    let mut after = Cursor::START;
    let mut paged = Vec::new();
    loop {
        let page =
            v.f.app
                .read(
                    Some(&v.reader),
                    requests::Inbox {
                        page: PageRequest {
                            limit: Some(1),
                            after,
                        },
                    },
                )
                .await
                .unwrap();
        assert_eq!(page.head, inbox.head);
        assert!(page.items.len() <= 1);
        paged.extend(page.items.into_iter().map(|item| item.dispatch.id));
        match page.next_cursor {
            Some(next) => {
                assert!(next > after);
                after = next;
            }
            None => {
                assert_eq!(page.scan_cursor, page.head);
                break;
            }
        }
    }
    assert_eq!(
        paged,
        inbox
            .items
            .iter()
            .map(|item| item.dispatch.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(inventory(&v.f).await, before);
}

#[tokio::test]
async fn later_notice_uses_dispatch_head_beyond_audit_head_and_respects_captured_snapshot() {
    let v = Views::new().await;
    let before = inventory(&v.f).await;
    let mut frozen = v.f.store.snapshot().await.unwrap();
    let frozen_audit = frozen.head();
    let frozen_dispatch = frozen.dispatch_head();
    let mut tx = v.f.store.begin().await.unwrap();
    let occurrence = audit(tx.as_mut(), v.author_id, v.f.clock.now()).await;
    tx.commit().await.unwrap();
    let audit_only = inventory(&v.f).await;
    assert_eq!(audit_only[0..5], before[0..5]);
    assert_eq!(audit_only[5], before[5] + 1);
    let control = v.notice(&[v.reader_id]).await;
    assert!(control.creation_seq > occurrence.seq);

    assert_eq!(frozen.head(), frozen_audit);
    assert_eq!(frozen.dispatch_head(), frozen_dispatch);
    assert!(
        !frozen
            .inbox(v.reader_id, Cursor::START, 200)
            .await
            .unwrap()
            .iter()
            .any(|row| row.dispatch.id == control.id)
    );
    assert!(
        frozen
            .events(frozen_audit, 200, None)
            .await
            .unwrap()
            .is_empty()
    );
    drop(frozen);
    let mut current = v.f.store.snapshot().await.unwrap();
    assert_eq!(current.head(), occurrence.seq.cursor());
    assert_eq!(current.dispatch_head(), control.creation_seq.cursor());
    assert!(current.dispatch_head() > current.head());
    let row = current
        .inbox(v.reader_id, occurrence.seq.cursor(), 200)
        .await
        .unwrap();
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].dispatch, control);
    assert_eq!(
        row[0].notice.as_ref().unwrap().kind,
        NoticeKind::AgentJoined
    );
    let events = current.events(frozen_audit, 200, None).await.unwrap();
    assert_eq!(events.as_slice(), std::slice::from_ref(&occurrence));
    drop(current);

    let observed_head = v.head().await;
    assert_eq!(observed_head.seq, occurrence.seq.cursor());
    assert_eq!(observed_head.event_id, Some(occurrence.event_id));
    let inbox =
        v.f.app
            .read(
                Some(&v.reader),
                requests::Inbox {
                    page: PageRequest {
                        after: occurrence.seq.cursor(),
                        limit: None,
                    },
                },
            )
            .await
            .unwrap();
    assert_eq!(inbox.head, control.creation_seq.cursor());
    assert_eq!(inbox.scan_cursor, control.creation_seq.cursor());
    assert_eq!(inbox.items.len(), 1);
    assert_eq!(inbox.items[0].dispatch.id, control.id);
    assert!(inbox.items[0].message.is_none());
    let after = inventory(&v.f).await;
    assert_eq!(after[0], before[0] + 1);
    assert_eq!(after[1], before[1]);
    assert_eq!(after[2], before[2] + 1);
    assert_eq!(after[3], before[3] + 1);
    assert_eq!(after[4], before[4] + 1);
    assert_eq!(after[5], before[5] + 1);
    assert_eq!(&after[6..], &before[6..]);
}

#[tokio::test]
async fn common_sequence_gaps_are_legal_but_missing_middle_audit_ordinal_requires_reset() {
    let v = Views::new().await;
    let initial = v.head().await;
    let before_deliveries = v.f.count("deliveries").await;
    let mut tx = v.f.store.begin().await.unwrap();
    let first = audit(tx.as_mut(), v.author_id, v.f.clock.now()).await;
    let first_notice = notice(tx.as_mut(), v.author_id, &[], v.f.clock.now()).await;
    let middle = audit(tx.as_mut(), v.author_id, v.f.clock.now()).await;
    let second_notice = notice(tx.as_mut(), v.author_id, &[], v.f.clock.now()).await;
    let last = audit(tx.as_mut(), v.author_id, v.f.clock.now()).await;
    tx.commit().await.unwrap();
    assert!(first.seq < first_notice.creation_seq && first_notice.creation_seq < middle.seq);
    assert!(middle.seq < second_notice.creation_seq && second_notice.creation_seq < last.seq);
    assert!(middle.seq.get() > first.seq.get() + 1);
    assert!(last.seq.get() > middle.seq.get() + 1);
    let mut ordinals = Vec::new();
    for event in [&first, &middle, &last] {
        let row =
            v.f.inspect
                .query_one(
                    &Query::select()
                        .column(Alias::new("ordinal"))
                        .column(Alias::new("event_id"))
                        .from(Alias::new("events"))
                        .and_where(Expr::col(Alias::new("seq")).eq(event.seq.get()))
                        .to_owned(),
                )
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            row.try_get::<String>("", "event_id").unwrap(),
            event.event_id.to_string()
        );
        ordinals.push(row.try_get::<i64>("", "ordinal").unwrap());
    }
    assert_eq!(ordinals[1], ordinals[0] + 1);
    assert_eq!(ordinals[2], ordinals[1] + 1);
    let complete =
        v.f.app
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
    assert!(complete.items.is_empty());
    assert!(!complete.has_more);
    assert_eq!(complete.scan, complete.head);
    assert_eq!(complete.head.seq, last.seq.cursor());
    assert_eq!(complete.head.event_id, Some(last.event_id));
    let mut snapshot = v.f.store.snapshot().await.unwrap();
    assert_eq!(
        snapshot.events(initial.seq, 200, None).await.unwrap(),
        [first.clone(), middle.clone(), last.clone()]
    );
    drop(snapshot);
    assert_eq!(v.f.count("deliveries").await, before_deliveries);

    // Delete a standalone middle audit row, leaving both public cursor anchors
    // and every Dispatch intact. Its private ordinal is now a real audit gap.
    let deleted =
        v.f.inspect
            .execute(
                &Query::delete()
                    .from_table(Alias::new("events"))
                    .and_where(Expr::col(Alias::new("seq")).eq(middle.seq.get()))
                    .to_owned(),
            )
            .await
            .unwrap();
    assert_eq!(deleted.rows_affected(), 1);
    let error =
        v.f.app
            .read(
                None,
                requests::ViewerChanges {
                    changes: ChangeRequest {
                        after: initial,
                        through: Some(complete.head),
                        limit: None,
                    },
                },
            )
            .await
            .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["reason"], "history_cursor_invalidated");
    assert_eq!(error.details["reset_required"], true);
    let mut tx = v.f.store.begin().await.unwrap();
    assert_eq!(tx.event(first.event_id).await.unwrap(), Some(first));
    assert_eq!(tx.event(last.event_id).await.unwrap(), Some(last));
    assert!(tx.event(middle.event_id).await.unwrap().is_none());
    assert_eq!(
        tx.dispatch(first_notice.id).await.unwrap(),
        Some(first_notice)
    );
    assert_eq!(
        tx.dispatch(second_notice.id).await.unwrap(),
        Some(second_notice)
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn message_header_precedes_its_creation_audit_and_changes_project_the_same_public_id() {
    let v = Views::new().await;
    let initial = v.head().await;
    let posted =
        v.f.post(&v.author, "complete body\0with retained interior")
            .await;
    let message_id = id(&posted.result, "message_id");
    let audit_seq: Sequence = serde_json::from_value(posted.result["seq"].clone()).unwrap();
    let mut tx = v.f.store.begin().await.unwrap();
    let header = tx.dispatch(message_id).await.unwrap().unwrap();
    let message = tx.message(message_id).await.unwrap().unwrap();
    assert_eq!(header.kind, DispatchKind::Message);
    assert_eq!(message.id, header.id);
    assert_eq!(message.creation_seq, header.creation_seq);
    assert!(header.creation_seq < audit_seq);
    tx.rollback().await.unwrap();
    let row =
        v.f.inspect
            .query_one(
                &Query::select()
                    .column(Alias::new("event_id"))
                    .column(Alias::new("message_id"))
                    .from(Alias::new("events"))
                    .and_where(Expr::col(Alias::new("seq")).eq(audit_seq.get()))
                    .and_where(Expr::col(Alias::new("kind")).eq("message.created"))
                    .to_owned(),
            )
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        row.try_get::<String>("", "message_id").unwrap(),
        message_id.to_string()
    );
    let audit_id = Uuid::parse_str(&row.try_get::<String>("", "event_id").unwrap()).unwrap();
    let later_notice = v.notice(&[]).await;
    assert!(later_notice.creation_seq > audit_seq);
    let before = inventory(&v.f).await;
    let changes =
        v.f.app
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
    assert_eq!(changes.items.len(), 1);
    assert_eq!(changes.items[0].event_id, audit_id);
    assert_eq!(changes.items[0].seq, audit_seq);
    assert_eq!(changes.head.seq, audit_seq.cursor());
    assert_eq!(changes.scan, changes.head);
    assert!(!changes.has_more);
    let ViewerChangeKind::MessageCreated { message: projected } = &changes.items[0].change else {
        panic!("creation audit must project its Message Header")
    };
    assert_eq!(projected.message.message, message);
    let detail =
        v.f.app
            .read(
                None,
                requests::Message {
                    message_id,
                    receipts: PageRequest::default(),
                },
            )
            .await
            .unwrap();
    assert_eq!(detail.message.message, message);
    let window =
        v.f.app
            .read(
                None,
                requests::RootWindow {
                    channel: "dev".into(),
                    window: WindowRequest {
                        snapshot: Some(changes.head),
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap();
    assert_eq!(window.items.len(), 1);
    assert_eq!(window.items[0].message.message, message);
    assert_eq!(inventory(&v.f).await, before);
}
