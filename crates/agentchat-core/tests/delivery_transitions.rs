//! Audit entries explain the current state and every immutable first fact.

use agentchat_core::{
    model::{Delivery, DeliveryHistory, DeliveryState as State, DeliveryTransition as Transition},
    time::Timestamp,
};
use uuid::Uuid;

fn time(seconds: i64) -> Timestamp {
    Timestamp::parse("2026-10-31T00:00:00.000Z")
        .unwrap()
        .after_seconds(seconds)
        .unwrap()
}

fn queued() -> Delivery {
    Delivery {
        id: Uuid::new_v4(),
        dispatch_id: Uuid::new_v4(),
        recipient_id: Uuid::new_v4(),
        state: State::Pending,
        history: DeliveryHistory::queued(time(0)),
        wake_eligible: false,
        wake_batch_id: None,
        attempts: 0,
    }
}

fn append(old: &Delivery, new: &Delivery, rows: &mut Vec<Transition>) {
    rows.extend(Transition::plan_update(old, new, rows.len() as u64).unwrap());
    Transition::validate_chain(new, rows.len() as u64, rows).unwrap();
}

fn processing() -> (Delivery, Vec<Transition>) {
    let pending = queued();
    let mut rows = vec![Transition::initial(&pending).unwrap()];
    let mut current = pending.clone();
    current.start_processing(time(1), 2).unwrap();
    append(&pending, &current, &mut rows);
    (current, rows)
}

#[test]
fn initial_entry_has_no_invented_previous_state_or_processing_deadline() {
    let delivery = queued();
    let row = Transition::initial(&delivery).unwrap();
    assert_eq!(row.delivery_id, delivery.id);
    assert_eq!(row.ordinal, 1);
    assert_eq!(row.from_state, None);
    assert_eq!(row.to_state, State::Pending);
    assert_eq!(row.changed_at, delivery.history.queued_at);
    assert_eq!(row.deadline_at, None);
    Transition::validate_chain(&delivery, 1, &[row]).unwrap();
    assert!(Transition::validate_chain(&delivery, 0, &[]).is_err());
    let (processing, _) = processing();
    assert!(Transition::initial(&processing).is_err());
}

#[test]
fn timely_history_explains_the_frozen_deadline_and_first_confirmation() {
    let (old, mut rows) = processing();
    assert_eq!(rows[1].deadline_at, Some(time(3)));
    let mut delivery = old.clone();
    delivery.confirm_delivery(time(3)).unwrap();
    append(&old, &delivery, &mut rows);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].from_state, Some(State::Processing));
    assert_eq!(rows[2].to_state, State::Delivered);
    assert_eq!(rows[2].changed_at, time(3));
    assert_eq!(rows[2].deadline_at, None);
    assert!(
        Transition::plan_update(&delivery, &delivery, 3)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn simultaneous_expiry_and_original_late_acceptance_have_two_ordered_entries() {
    let (old, mut rows) = processing();
    let mut delivery = old.clone();
    delivery.confirm_delivery(time(5)).unwrap();
    append(&old, &delivery, &mut rows);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[2].to_state, State::FailedTimeout);
    assert_eq!(rows[3].from_state, Some(State::FailedTimeout));
    assert_eq!(rows[3].to_state, State::DeliveredLate);
    assert_eq!(rows[2].changed_at, time(5));
    assert_eq!(rows[3].changed_at, time(5));
    assert_ne!(rows[2].id, rows[3].id);
    assert_eq!((rows[2].ordinal, rows[3].ordinal), (3, 4));
    let mut skipped = rows.clone();
    skipped.remove(2);
    skipped[2].ordinal = 3;
    skipped[2].from_state = Some(State::Processing);
    assert!(Transition::validate_chain(&delivery, 3, &skipped).is_err());
}

#[test]
fn observed_timeout_and_later_completion_keep_their_distinct_facts() {
    let (old, mut rows) = processing();
    let mut expired = old.clone();
    expired.expire(time(5)).unwrap();
    append(&old, &expired, &mut rows);
    assert_eq!(expired.history.delivered_at, None);
    let mut late = expired.clone();
    late.confirm_delivery(time(9)).unwrap();
    append(&expired, &late, &mut rows);
    assert_eq!(rows[2].changed_at, time(5));
    assert_eq!(rows[3].changed_at, time(9));
    assert_eq!(late.history.deadline_at, Some(time(3)));
}

#[test]
fn departure_before_or_during_processing_retains_terminal_absence() {
    for started in [false, true] {
        let (old, mut rows) = if started {
            processing()
        } else {
            let delivery = queued();
            let rows = vec![Transition::initial(&delivery).unwrap()];
            (delivery, rows)
        };
        let mut left = old.clone();
        left.depart(time(5)).unwrap();
        append(&old, &left, &mut rows);
        assert_eq!(rows.last().unwrap().to_state, State::FailedLeft);
        assert_eq!(left.history.processing_started_at.is_some(), started);
        assert!(
            Transition::plan_update(&left, &left, rows.len() as u64)
                .unwrap()
                .is_empty()
        );
        let mut changed = left.clone();
        changed.history.processing_started_at = Some(time(1));
        changed.history.deadline_at = Some(time(3));
        if !started {
            assert!(Transition::plan_update(&left, &changed, 2).is_err());
        }
    }
}

#[test]
fn invalid_chain_identity_order_state_deadline_and_projection_are_rejected() {
    let (delivery, rows) = processing();
    for mutation in 0..9 {
        let mut broken = rows.clone();
        match mutation {
            0 => broken[1].id = broken[0].id,
            1 => broken[1].delivery_id = Uuid::new_v4(),
            2 => broken[1].ordinal = 3,
            3 => broken[1].from_state = None,
            4 => broken[0].from_state = Some(State::Pending),
            5 => broken[1].deadline_at = None,
            6 => broken[1].deadline_at = Some(time(1)),
            7 => broken[0].deadline_at = Some(time(3)),
            _ => broken[1].to_state = State::Pending,
        }
        assert!(
            Transition::validate_chain(&delivery, 2, &broken).is_err(),
            "mutation {mutation}"
        );
    }
    let mut wrong_projection = delivery.clone();
    wrong_projection.history.deadline_at = Some(time(4));
    assert!(Transition::validate_chain(&wrong_projection, 2, &rows).is_err());
    assert!(Transition::validate_chain(&delivery, 1, &rows).is_err());
    let mut oversized = rows.clone();
    oversized.extend(rows.clone());
    oversized.push(rows[0].clone());
    assert!(Transition::validate_chain(&delivery, 5, &oversized).is_err());
}

#[test]
fn attempt_only_retries_are_noops_but_foreign_identity_and_version_are_not() {
    let (old, _) = processing();
    let mut retried = old.clone();
    retried.attempts += 1;
    assert!(
        Transition::plan_update(&old, &retried, 2)
            .unwrap()
            .is_empty()
    );
    for mutation in 0..3 {
        let mut foreign = retried.clone();
        match mutation {
            0 => foreign.id = Uuid::new_v4(),
            1 => foreign.dispatch_id = Uuid::new_v4(),
            _ => foreign.recipient_id = Uuid::new_v4(),
        }
        assert!(Transition::plan_update(&old, &foreign, 2).is_err());
    }
    assert!(Transition::plan_update(&old, &retried, 0).is_err());
    assert!(Transition::plan_update(&old, &retried, 3).is_err());
    let encoded = serde_json::to_value(&old).unwrap();
    assert!(encoded.get("state_version").is_none());
    assert!(encoded.get("transitions").is_none());
}

#[test]
fn transition_observation_times_cannot_move_backwards() {
    let pending = queued();
    let mut started = pending.clone();
    assert!(started.start_processing(time(-1), 2).is_err());
    let (old, mut rows) = processing();
    let mut left = old.clone();
    left.depart(time(1)).unwrap();
    append(&old, &left, &mut rows);
    assert_eq!(rows[1].changed_at, rows[2].changed_at);
    let mut backwards = rows.clone();
    backwards[2].changed_at = time(0);
    assert!(Transition::validate_chain(&left, 3, &backwards).is_err());
}
