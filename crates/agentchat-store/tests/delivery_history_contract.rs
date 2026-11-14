//! Real persisted history, private versions and projections share one boundary.
mod support;

use agentchat_core::{
    application::Mutation,
    model::{
        Delivery, DeliveryHistory, DeliveryState as State, DeliveryTransition, DispatchKind,
        DispatchPayload, Message,
    },
    ports::Storage,
    time::{Clock, Timestamp},
};
use agentchat_store::Store;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbErr,
    sea_query::{Alias, Asterisk, Expr, ExprTrait, Order, Query},
};
use support::Fixture;
use uuid::Uuid;

async fn setup() -> (Fixture, Uuid, Uuid, Uuid) {
    let f = Fixture::new().await;
    let author = f.join(&f.context(), "author").await;
    let recipient = f.join(&f.context(), "recipient").await;
    let mut tx = f.store.begin().await.unwrap();
    let channel = tx.channel("dev").await.unwrap().unwrap().id;
    tx.rollback().await.unwrap();
    (f, author, recipient, channel)
}

async fn queued(f: &Fixture, author: Uuid, recipient: Uuid, channel: Uuid) -> Delivery {
    let mut tx = f.store.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
        .await
        .unwrap();
    let payload = DispatchPayload::Message(Message {
        id: header.id,
        channel_id: channel,
        author_id: author,
        root_message_id: None,
        body: "history contract".into(),
        created_at: header.created_at,
        creation_seq: header.creation_seq,
    });
    let delivery = Delivery {
        id: Uuid::new_v4(),
        dispatch_id: header.id,
        recipient_id: recipient,
        state: State::Pending,
        wake_eligible: false,
        wake_batch_id: None,
        attempts: 0,
        history: DeliveryHistory::queued(header.created_at),
    };
    tx.publish_dispatch(&header, &payload, std::slice::from_ref(&delivery))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    delivery
}

async fn save(f: &Fixture, value: &Delivery) {
    let mut tx = f.store.begin().await.unwrap();
    tx.save_delivery(value).await.unwrap();
    tx.commit().await.unwrap();
}

async fn reopened(f: &Fixture, id: Uuid) -> Delivery {
    let store = Store::open(
        f.root.path().join("agentchat.sqlite3"),
        f.store.identity().clone(),
    )
    .await
    .unwrap();
    let mut tx = store.begin().await.unwrap();
    let value = tx.delivery(id).await.unwrap().unwrap();
    tx.rollback().await.unwrap();
    store.close().await.unwrap();
    value
}

async fn history(f: &Fixture, id: Uuid) -> Vec<DeliveryTransition> {
    f.inspect
        .query_all(
            &Query::select()
                .column(Asterisk)
                .from(Alias::new("delivery_state_transitions"))
                .and_where(Expr::col(Alias::new("delivery_id")).eq(id.to_string()))
                .order_by(Alias::new("ordinal"), Order::Asc)
                .to_owned(),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| DeliveryTransition {
            id: Uuid::parse_str(&row.try_get::<String>("", "id").unwrap()).unwrap(),
            delivery_id: Uuid::parse_str(&row.try_get::<String>("", "delivery_id").unwrap())
                .unwrap(),
            ordinal: u64::try_from(row.try_get::<i64>("", "ordinal").unwrap()).unwrap(),
            from_state: row
                .try_get::<Option<String>>("", "from_state")
                .unwrap()
                .map(|value| value.parse().unwrap()),
            to_state: row
                .try_get::<String>("", "to_state")
                .unwrap()
                .parse()
                .unwrap(),
            changed_at: Timestamp::parse(&row.try_get::<String>("", "changed_at").unwrap())
                .unwrap(),
            deadline_at: row
                .try_get::<Option<String>>("", "deadline_at")
                .unwrap()
                .map(|value| Timestamp::parse(&value).unwrap()),
        })
        .collect()
}

async fn assert_projection(f: &Fixture, value: &Delivery, version: i64) {
    let row = f
        .inspect
        .query_one(
            &Query::select()
                .column(Asterisk)
                .from(Alias::new("deliveries"))
                .and_where(Expr::col(Alias::new("id")).eq(value.id.to_string()))
                .to_owned(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<String>("", "state").unwrap(),
        value.state.as_str()
    );
    assert_eq!(row.try_get::<i64>("", "state_version").unwrap(), version);
    assert_eq!(
        row.try_get::<String>("", "queued_at").unwrap(),
        value.history.queued_at.to_string()
    );
    for (column, expected) in [
        ("processing_started_at", value.history.processing_started_at),
        ("deadline_at", value.history.deadline_at),
        ("timed_out_at", value.history.timed_out_at),
        ("delivered_at", value.history.delivered_at),
        ("left_at", value.history.left_at),
    ] {
        assert_eq!(
            row.try_get::<Option<String>>("", column).unwrap(),
            expected.map(|at| at.to_string()),
            "{column}"
        );
    }
}

async fn assert_chain(f: &Fixture, value: &Delivery, states: &[State]) -> Vec<DeliveryTransition> {
    let rows = history(f, value.id).await;
    assert_eq!(
        rows.iter().map(|row| row.to_state).collect::<Vec<_>>(),
        states
    );
    assert_eq!(rows[0].from_state, None);
    assert_eq!(rows[0].changed_at, value.history.queued_at);
    assert_eq!(rows[0].deadline_at, None);
    let mut ids = std::collections::BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        assert!(!row.id.is_nil());
        assert!(ids.insert(row.id));
        assert_eq!(row.delivery_id, value.id);
        assert_eq!(row.ordinal, index as u64 + 1);
        assert_eq!(
            row.changed_at,
            match row.to_state {
                State::Pending => value.history.queued_at,
                State::Processing => value.history.processing_started_at.unwrap(),
                State::Delivered | State::DeliveredLate => value.history.delivered_at.unwrap(),
                State::FailedTimeout => value.history.timed_out_at.unwrap(),
                State::FailedLeft => value.history.left_at.unwrap(),
            }
        );
        if index > 0 {
            assert_eq!(row.from_state, Some(states[index - 1]));
            assert!(row.changed_at >= rows[index - 1].changed_at);
        }
        assert_eq!(
            row.deadline_at,
            if row.to_state == State::Processing {
                value.history.deadline_at
            } else {
                None
            }
        );
    }
    assert_projection(f, value, states.len() as i64).await;
    assert_eq!(reopened(f, value.id).await, *value);
    rows
}

#[tokio::test]
async fn initial_row_is_durable_and_noop_or_attempt_only_saves_append_zero_rows() {
    let (f, author, actor, channel) = setup().await;
    let mut value = queued(&f, author, actor, channel).await;
    let initial = assert_chain(&f, &value, &[State::Pending]).await;
    f.clock.advance(100);
    save(&f, &value).await;
    value.attempts = 1;
    save(&f, &value).await;
    value.attempts = 2;
    save(&f, &value).await;
    assert_eq!(history(&f, value.id).await, initial);
    assert_chain(&f, &value, &[State::Pending]).await;
    assert_eq!(value.history.processing_started_at, None);
    assert_eq!(value.history.deadline_at, None);
}

#[tokio::test]
async fn explicit_and_dropped_outer_rollbacks_preserve_state_version_and_initial_identity() {
    let (f, author, actor, channel) = setup().await;
    let original = queued(&f, author, actor, channel).await;
    let initial = history(&f, original.id).await;
    for explicit in [true, false] {
        let mut processing = original.clone();
        processing
            .start_processing(f.clock.now().after_seconds(1).unwrap(), 10)
            .unwrap();
        let mut tx = f.store.begin().await.unwrap();
        tx.save_delivery(&processing).await.unwrap();
        assert_eq!(tx.delivery(original.id).await.unwrap(), Some(processing));
        if explicit {
            tx.rollback().await.unwrap();
        } else {
            drop(tx);
        }
        assert_eq!(reopened(&f, original.id).await, original);
        assert_eq!(history(&f, original.id).await, initial);
        assert_projection(&f, &original, 1).await;
    }
}

#[tokio::test]
async fn stale_snapshot_and_rejected_projection_save_can_be_ignored_then_committed() {
    let (f, author, actor, channel) = setup().await;
    let stale = queued(&f, author, actor, channel).await;
    let mut processing = stale.clone();
    processing
        .start_processing(f.clock.now().after_seconds(1).unwrap(), 10)
        .unwrap();
    save(&f, &processing).await;
    let before = assert_chain(&f, &processing, &[State::Pending, State::Processing]).await;
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.save_delivery(&stale).await.is_err());
    tx.commit().await.unwrap();
    let mut invalid = processing.clone();
    invalid.history.deadline_at = Some(f.clock.now().after_seconds(100).unwrap());
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.save_delivery(&invalid).await.is_err());
    tx.commit().await.unwrap();

    // This is a valid state transition with an invalid bookkeeping FK. Even
    // after ignored driver failure, neither its projection nor history survives.
    let mut failed_save = processing.clone();
    failed_save
        .confirm_delivery(f.clock.now().after_seconds(2).unwrap())
        .unwrap();
    failed_save.wake_batch_id = Some(Uuid::new_v4());
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.save_delivery(&failed_save).await.is_err());
    tx.commit().await.unwrap();
    assert_eq!(reopened(&f, processing.id).await, processing);
    assert_eq!(history(&f, processing.id).await, before);
    assert_projection(&f, &processing, 2).await;
}

#[tokio::test]
async fn late_first_observation_commits_two_ordered_rows_at_one_time_and_replays_zero() {
    let (f, author, actor, channel) = setup().await;
    let mut value = queued(&f, author, actor, channel).await;
    let start = f.clock.now().after_seconds(1).unwrap();
    value.start_processing(start, 10).unwrap();
    save(&f, &value).await;
    let prefix = history(&f, value.id).await;
    let observation = start.after_seconds(11).unwrap();
    value.confirm_delivery(observation).unwrap();
    let mut tx = f.store.begin().await.unwrap();
    tx.save_delivery(&value).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(history(&f, value.id).await, prefix);
    save(&f, &value).await;
    let committed = assert_chain(
        &f,
        &value,
        &[
            State::Pending,
            State::Processing,
            State::FailedTimeout,
            State::DeliveredLate,
        ],
    )
    .await;
    assert_eq!(&committed[..2], prefix.as_slice());
    assert_eq!(committed[2].changed_at, observation);
    assert_eq!(committed[3].changed_at, observation);
    assert_eq!(value.history.timed_out_at, Some(observation));
    assert_eq!(value.history.delivered_at, Some(observation));
    assert_ne!(committed[2].id, committed[3].id);
    assert!(
        !value
            .confirm_delivery(observation.after_seconds(1).unwrap())
            .unwrap()
    );
    save(&f, &value).await;
    assert_eq!(history(&f, value.id).await, committed);
}

#[tokio::test]
async fn separately_observed_timeout_retains_its_first_fact_when_later_success_arrives() {
    let (f, author, actor, channel) = setup().await;
    let mut value = queued(&f, author, actor, channel).await;
    let start = f.clock.now().after_seconds(1).unwrap();
    value.start_processing(start, 10).unwrap();
    save(&f, &value).await;
    assert!(!value.expire(start.after_seconds(10).unwrap()).unwrap());
    save(&f, &value).await;
    assert_eq!(history(&f, value.id).await.len(), 2);
    let timeout = start.after_seconds(11).unwrap();
    value.expire(timeout).unwrap();
    save(&f, &value).await;
    let timeout_rows = assert_chain(
        &f,
        &value,
        &[State::Pending, State::Processing, State::FailedTimeout],
    )
    .await;
    let stale = value.clone();
    value
        .confirm_delivery(start.after_seconds(20).unwrap())
        .unwrap();
    save(&f, &value).await;
    let completed = assert_chain(
        &f,
        &value,
        &[
            State::Pending,
            State::Processing,
            State::FailedTimeout,
            State::DeliveredLate,
        ],
    )
    .await;
    assert_eq!(&completed[..3], timeout_rows.as_slice());
    assert_eq!(completed[2].changed_at, timeout);
    assert_eq!(completed[3].changed_at, start.after_seconds(20).unwrap());
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.save_delivery(&stale).await.is_err());
    tx.commit().await.unwrap();
    assert_eq!(history(&f, value.id).await, completed);
    assert_eq!(reopened(&f, value.id).await, value);
}

#[tokio::test]
async fn all_terminal_histories_refuse_rewrites_including_first_processing_after_pending_leave() {
    let (f, author, actor, channel) = setup().await;
    for terminal in [State::FailedLeft, State::Delivered, State::DeliveredLate] {
        let mut value = queued(&f, author, actor, channel).await;
        let start = f.clock.now().after_seconds(1).unwrap();
        if terminal == State::FailedLeft {
            value.depart(start.after_seconds(2).unwrap()).unwrap();
        } else {
            value.start_processing(start, 10).unwrap();
            save(&f, &value).await;
            value
                .confirm_delivery(
                    start
                        .after_seconds(if terminal == State::Delivered { 2 } else { 11 })
                        .unwrap(),
                )
                .unwrap();
        }
        save(&f, &value).await;
        let before = history(&f, value.id).await;
        let mut forged = value.clone();
        if terminal == State::FailedLeft {
            // Both values form a valid FailedLeft shape, but terminal absence
            // of a processing claim is already committed history.
            forged.history.processing_started_at = Some(start);
            forged.history.deadline_at = Some(start.after_seconds(10).unwrap());
        } else {
            forged.history.delivered_at = Some(
                value
                    .history
                    .delivered_at
                    .unwrap()
                    .after_seconds(1)
                    .unwrap(),
            );
        }
        forged.history.validate(forged.state).unwrap();
        let mut tx = f.store.begin().await.unwrap();
        assert!(tx.save_delivery(&forged).await.is_err(), "{terminal:?}");
        tx.commit().await.unwrap();
        let mut resurrection = value.clone();
        resurrection.state = State::Pending;
        resurrection.history = DeliveryHistory::queued(value.history.queued_at);
        let mut tx = f.store.begin().await.unwrap();
        assert!(
            tx.save_delivery(&resurrection).await.is_err(),
            "{terminal:?}"
        );
        tx.commit().await.unwrap();
        assert_eq!(reopened(&f, value.id).await, value);
        assert_eq!(history(&f, value.id).await, before);
        assert_projection(&f, &value, before.len() as i64).await;
    }
}

#[tokio::test]
async fn bulk_departure_appends_for_pending_processing_only_and_replay_appends_zero() {
    let (f, author, actor, channel) = setup().await;
    let other = f.join(&f.context(), "other").await;
    let mut values = Vec::new();
    for state in [
        State::Pending,
        State::Processing,
        State::Delivered,
        State::FailedTimeout,
        State::DeliveredLate,
    ] {
        let mut value = queued(&f, author, actor, channel).await;
        let start = f.clock.now().after_seconds(1).unwrap();
        if state != State::Pending {
            value.start_processing(start, 10).unwrap();
            save(&f, &value).await;
        }
        match state {
            State::Delivered => {
                value
                    .confirm_delivery(start.after_seconds(2).unwrap())
                    .unwrap();
            }
            State::FailedTimeout => {
                value.expire(start.after_seconds(11).unwrap()).unwrap();
            }
            State::DeliveredLate => {
                value
                    .confirm_delivery(start.after_seconds(11).unwrap())
                    .unwrap();
            }
            _ => {}
        }
        save(&f, &value).await;
        values.push(value);
    }
    let untouched = queued(&f, author, other, channel).await;
    let mut before = Vec::new();
    for value in &values {
        before.push(history(&f, value.id).await);
    }
    let at = f.clock.now().after_seconds(30).unwrap();
    let mut tx = f.store.begin().await.unwrap();
    tx.cancel_deliveries(actor, at).await.unwrap();
    tx.commit().await.unwrap();
    let mut committed = Vec::new();
    for (index, expected) in values.iter_mut().enumerate() {
        let changed = expected.depart(at).unwrap();
        let rows = history(&f, expected.id).await;
        assert_eq!(&rows[..before[index].len()], before[index].as_slice());
        assert_eq!(rows.len(), before[index].len() + usize::from(changed));
        if changed {
            let last = rows.last().unwrap();
            assert_eq!(last.to_state, State::FailedLeft);
            assert_eq!(last.changed_at, at);
            assert_eq!(last.deadline_at, None);
        }
        assert_eq!(reopened(&f, expected.id).await, *expected);
        assert_projection(&f, expected, rows.len() as i64).await;
        committed.push(rows);
    }
    let mut tx = f.store.begin().await.unwrap();
    tx.cancel_deliveries(actor, at.after_seconds(1).unwrap())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    for (index, expected) in values.iter().enumerate() {
        assert_eq!(history(&f, expected.id).await, committed[index]);
    }
    assert_chain(&f, &untouched, &[State::Pending]).await;
}

#[tokio::test]
async fn channel_departure_preserves_other_channel_and_recipient_histories() {
    let (f, author, actor, channel) = setup().await;
    let creator = f.context();
    f.join(&creator, "creator").await;
    f.run(
        &creator,
        Mutation::ChannelCreate {
            name: "side".into(),
        },
    )
    .await;
    let mut tx = f.store.begin().await.unwrap();
    let side = tx.channel("side").await.unwrap().unwrap().id;
    tx.rollback().await.unwrap();
    let mut target = queued(&f, author, actor, channel).await;
    let other_channel = queued(&f, author, actor, side).await;
    let other_recipient = queued(&f, author, author, channel).await;
    let before_target = history(&f, target.id).await;
    let before_channel = history(&f, other_channel.id).await;
    let before_recipient = history(&f, other_recipient.id).await;
    let at = f.clock.now().after_seconds(1).unwrap();
    let mut tx = f.store.begin().await.unwrap();
    tx.cancel_channel_deliveries(channel, actor, at)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    target.depart(at).unwrap();
    let rows = assert_chain(&f, &target, &[State::Pending, State::FailedLeft]).await;
    assert_eq!(&rows[..1], before_target.as_slice());
    assert_eq!(reopened(&f, other_channel.id).await, other_channel);
    assert_eq!(reopened(&f, other_recipient.id).await, other_recipient);
    assert_eq!(history(&f, other_channel.id).await, before_channel);
    assert_eq!(history(&f, other_recipient.id).await, before_recipient);
}

#[tokio::test]
async fn persisted_ordinal_gap_wrong_predecessor_version_and_projection_are_not_guessed() {
    for corruption in ["ordinal_gap", "predecessor", "version", "projection"] {
        let (f, author, actor, channel) = setup().await;
        let mut value = queued(&f, author, actor, channel).await;
        let start = f.clock.now().after_seconds(1).unwrap();
        value.start_processing(start, 10).unwrap();
        save(&f, &value).await;
        if corruption == "predecessor" {
            value
                .confirm_delivery(start.after_seconds(1).unwrap())
                .unwrap();
            save(&f, &value).await;
        }
        let mut update = Query::update();
        match corruption {
            "ordinal_gap" => {
                update
                    .table(Alias::new("delivery_state_transitions"))
                    .value(Alias::new("ordinal"), 3_i64)
                    .and_where(Expr::col(Alias::new("delivery_id")).eq(value.id.to_string()))
                    .and_where(Expr::col(Alias::new("ordinal")).eq(2_i64));
            }
            "predecessor" => {
                // Each row remains individually legal to the relational CHECK;
                // its predecessor is nevertheless incompatible with the chain.
                update
                    .table(Alias::new("delivery_state_transitions"))
                    .value(Alias::new("from_state"), "pending")
                    .value(Alias::new("to_state"), "failed_left")
                    .and_where(Expr::col(Alias::new("delivery_id")).eq(value.id.to_string()))
                    .and_where(Expr::col(Alias::new("ordinal")).eq(3_i64));
            }
            "version" => {
                update
                    .table(Alias::new("deliveries"))
                    .value(Alias::new("state_version"), 3_i64)
                    .and_where(Expr::col(Alias::new("id")).eq(value.id.to_string()));
            }
            "projection" => {
                update
                    .table(Alias::new("deliveries"))
                    .value(
                        Alias::new("queued_at"),
                        value
                            .history
                            .queued_at
                            .after_seconds(-1)
                            .unwrap()
                            .to_string(),
                    )
                    .and_where(Expr::col(Alias::new("id")).eq(value.id.to_string()));
            }
            _ => unreachable!(),
        }
        assert_eq!(
            f.inspect
                .execute(&update.to_owned())
                .await
                .unwrap()
                .rows_affected(),
            1
        );
        let mut tx = f.store.begin().await.unwrap();
        assert!(tx.delivery(value.id).await.is_err(), "{corruption}");
        assert!(tx.save_delivery(&value).await.is_err(), "{corruption}");
        tx.rollback().await.unwrap();
    }
}

async fn insert_history(db: &DatabaseConnection, row: &DeliveryTransition) -> Result<(), DbErr> {
    db.execute(
        &Query::insert()
            .into_table(Alias::new("delivery_state_transitions"))
            .columns(
                [
                    "id",
                    "delivery_id",
                    "ordinal",
                    "from_state",
                    "to_state",
                    "changed_at",
                    "deadline_at",
                ]
                .map(Alias::new),
            )
            .values_panic([
                row.id.to_string().into(),
                row.delivery_id.to_string().into(),
                (row.ordinal as i64).into(),
                row.from_state.map(|state| state.as_str().to_owned()).into(),
                row.to_state.as_str().into(),
                row.changed_at.to_string().into(),
                row.deadline_at.map(|at| at.to_string()).into(),
            ])
            .to_owned(),
    )
    .await
    .map(|_| ())
}

#[tokio::test]
async fn history_fk_ordinal_uniqueness_and_processing_only_deadline_are_relational_guards() {
    let (f, author, actor, channel) = setup().await;
    let value = queued(&f, author, actor, channel).await;
    let before = history(&f, value.id).await;
    let mut duplicate = before[0].clone();
    duplicate.id = Uuid::new_v4();
    assert!(insert_history(&f.inspect, &duplicate).await.is_err());
    duplicate.delivery_id = Uuid::new_v4();
    assert!(insert_history(&f.inspect, &duplicate).await.is_err());
    duplicate.delivery_id = value.id;
    duplicate.ordinal = 2;
    duplicate.from_state = Some(State::Pending);
    duplicate.to_state = State::FailedLeft;
    duplicate.deadline_at = Some(f.clock.now().after_seconds(1).unwrap());
    assert!(insert_history(&f.inspect, &duplicate).await.is_err());
    duplicate.to_state = State::Processing;
    duplicate.deadline_at = None;
    assert!(insert_history(&f.inspect, &duplicate).await.is_err());
    assert_eq!(history(&f, value.id).await, before);
    assert_chain(&f, &value, &[State::Pending]).await;
}
