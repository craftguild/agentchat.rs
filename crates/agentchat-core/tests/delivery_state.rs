//! Exact Enum, first-time history and legal lifecycle transitions.
use agentchat_core::{
    model::{Delivery, DeliveryHistory, DeliveryState as State},
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

#[test]
fn enum_has_only_the_six_exact_wire_and_relational_values() {
    for (state, wire) in [
        (State::Pending, "pending"),
        (State::Processing, "processing"),
        (State::Delivered, "delivered"),
        (State::FailedTimeout, "failed_timeout"),
        (State::FailedLeft, "failed_left"),
        (State::DeliveredLate, "delivered_late"),
    ] {
        assert_eq!(state.as_str(), wire);
        assert_eq!(wire.parse::<State>().unwrap(), state);
        let encoded = serde_json::to_string(&state).unwrap();
        assert_eq!(encoded, format!("\"{wire}\""));
        assert_eq!(serde_json::from_str::<State>(&encoded).unwrap(), state);
    }
    for invalid in [
        "",
        "inflight",
        "acknowledged",
        "cancelled",
        "FAILED_TIMEOUT",
        "failed-timeout",
        "delivered ",
    ] {
        assert!(invalid.parse::<State>().is_err());
        assert!(serde_json::from_value::<State>(serde_json::json!(invalid)).is_err());
    }
}

#[test]
fn pending_delay_and_retries_never_consume_or_extend_the_processing_interval() {
    let mut delivery = queued();
    assert!(!delivery.expire(time(10_000)).unwrap());
    assert!(delivery.start_processing(time(10_000), 3_600).unwrap());
    assert_eq!(delivery.history.processing_started_at, Some(time(10_000)));
    assert_eq!(delivery.history.deadline_at, Some(time(13_600)));
    let original = delivery.clone();
    assert!(!delivery.start_processing(time(11_000), 1).unwrap());
    assert_eq!(delivery, original);
    assert!(!delivery.expire(time(13_600)).unwrap());
    assert!(delivery.confirm_delivery(time(13_600)).unwrap());
    assert_eq!(delivery.state, State::Delivered);
    assert_eq!(delivery.history.delivered_at, Some(time(13_600)));
    assert_eq!(delivery.history.timed_out_at, None);
    let original = delivery.clone();
    assert!(!delivery.confirm_delivery(time(20_000)).unwrap());
    assert!(!delivery.depart(time(20_000)).unwrap());
    assert_eq!(delivery, original);
}

#[test]
fn delayed_expiry_and_original_late_result_retain_both_observed_times() {
    let mut delivery = queued();
    delivery.start_processing(time(1), 2).unwrap();
    assert!(delivery.expire(time(5)).unwrap());
    assert_eq!(delivery.state, State::FailedTimeout);
    assert_eq!(delivery.history.timed_out_at, Some(time(5)));
    assert!(!delivery.expire(time(6)).unwrap());
    assert!(!delivery.depart(time(6)).unwrap());
    assert!(!delivery.start_processing(time(6), 100).unwrap());
    assert!(delivery.confirm_delivery(time(7)).unwrap());
    assert_eq!(delivery.state, State::DeliveredLate);
    assert_eq!(delivery.history.timed_out_at, Some(time(5)));
    assert_eq!(delivery.history.delivered_at, Some(time(7)));
    assert_eq!(delivery.history.deadline_at, Some(time(3)));
}

#[test]
fn late_result_materializes_timeout_without_waiting_for_maintenance() {
    let mut delivery = queued();
    delivery.start_processing(time(1), 2).unwrap();
    delivery.confirm_delivery(time(4)).unwrap();
    assert_eq!(delivery.state, State::DeliveredLate);
    assert_eq!(delivery.history.timed_out_at, Some(time(4)));
    assert_eq!(delivery.history.delivered_at, Some(time(4)));
    let first = delivery.clone();
    assert!(!delivery.expire(time(5)).unwrap());
    assert!(!delivery.confirm_delivery(time(6)).unwrap());
    assert_eq!(delivery, first);
}

#[test]
fn departure_before_or_during_processing_is_terminal_without_fabricated_acceptance() {
    for processing in [false, true] {
        let mut delivery = queued();
        assert!(delivery.confirm_delivery(time(0)).is_err());
        if processing {
            delivery.start_processing(time(1), 2).unwrap();
        }
        assert!(delivery.depart(time(2)).unwrap());
        assert_eq!(delivery.state, State::FailedLeft);
        assert_eq!(delivery.history.left_at, Some(time(2)));
        assert_eq!(delivery.history.delivered_at, None);
        assert_eq!(delivery.history.timed_out_at, None);
        let first = delivery.clone();
        assert!(!delivery.start_processing(time(3), 100).unwrap());
        assert!(!delivery.depart(time(3)).unwrap());
        assert!(delivery.confirm_delivery(time(3)).is_err());
        assert_eq!(delivery, first);
    }
}

#[test]
fn invalid_history_and_rewritten_first_facts_are_rejected() {
    let mut delivery = queued();
    assert!(delivery.start_processing(time(0), 0).is_err());
    assert!(delivery.start_processing(time(0), 2_592_001).is_err());
    delivery.start_processing(time(1), 2).unwrap();
    let first = delivery.history.clone();
    for field in [0, 1, 2] {
        let mut changed = first.clone();
        match field {
            0 => changed.queued_at = time(1),
            1 => changed.processing_started_at = Some(time(2)),
            _ => changed.deadline_at = Some(time(4)),
        }
        assert!(
            changed
                .validate_update(State::Processing, &first, State::Processing)
                .is_err()
        );
    }
    for state in [
        State::Pending,
        State::Delivered,
        State::FailedTimeout,
        State::FailedLeft,
        State::DeliveredLate,
    ] {
        assert!(first.validate(state).is_err());
    }
    let mut malformed = first.clone();
    malformed.deadline_at = None;
    assert!(malformed.validate(State::Processing).is_err());
    let mut malformed = first.clone();
    malformed.timed_out_at = Some(time(3));
    assert!(malformed.validate(State::FailedTimeout).is_err());
    delivery.expire(time(4)).unwrap();
    let mut rewritten = delivery.history.clone();
    rewritten.timed_out_at = Some(time(5));
    assert!(
        rewritten
            .validate_update(
                State::FailedTimeout,
                &delivery.history,
                State::FailedTimeout
            )
            .is_err()
    );
    assert!(
        first
            .validate_update(State::Processing, &delivery.history, State::FailedTimeout)
            .is_err()
    );
}

#[test]
fn canonical_wire_receipt_rejects_invalid_state_history_without_another_projection() {
    use agentchat_core::read::ReceiptDelivery;
    let mut delivery = queued();
    delivery.start_processing(time(1), 2).unwrap();
    let receipt = ReceiptDelivery {
        delivery_id: delivery.id,
        state: delivery.state,
        history: delivery.history,
    };
    let wire = serde_json::to_value(&receipt).unwrap();
    assert_eq!(wire["state"], "processing");
    assert_eq!(wire["queued_at"], time(0).to_string());
    assert!(wire.get("confirmation").is_none());
    assert!(wire.get("accepted_at").is_none());
    assert!(wire.get("acknowledged_at").is_none());
    assert_eq!(
        serde_json::from_value::<ReceiptDelivery>(wire.clone()).unwrap(),
        receipt
    );
    for field in ["processing_started_at", "deadline_at"] {
        let mut malformed = wire.clone();
        malformed[field] = serde_json::Value::Null;
        assert!(serde_json::from_value::<ReceiptDelivery>(malformed).is_err());
    }
    for state in [
        "delivered",
        "failed_timeout",
        "delivered_late",
        "acknowledged",
    ] {
        let mut malformed = wire.clone();
        malformed["state"] = state.into();
        assert!(serde_json::from_value::<ReceiptDelivery>(malformed).is_err());
    }
}

#[test]
fn full_delivery_wire_validates_the_same_history_invariant() {
    let delivery = queued();
    let wire = serde_json::to_value(&delivery).unwrap();
    assert_eq!(
        serde_json::from_value::<Delivery>(wire.clone()).unwrap(),
        delivery
    );
    let mut forged = wire;
    forged["state"] = "processing".into();
    assert!(serde_json::from_value::<Delivery>(forged).is_err());
}

#[test]
fn departed_history_cannot_gain_a_first_processing_pair_after_becoming_terminal() {
    for processing in [false, true] {
        let mut delivery = queued();
        if processing {
            delivery.start_processing(time(0), 10).unwrap();
        }
        delivery.depart(time(2)).unwrap();
        let original = delivery.history.clone();
        assert_eq!(original.processing_started_at.is_some(), processing);
        assert_eq!(original.deadline_at.is_some(), processing);
        original
            .validate_update(State::FailedLeft, &original, State::FailedLeft)
            .unwrap();
        let mut forged = original.clone();
        forged.processing_started_at = Some(time(1));
        forged.deadline_at = Some(time(11));
        // Both departure snapshots are structurally valid. A later update must
        // neither invent a missing claim nor replace the already retained one.
        forged.validate(State::FailedLeft).unwrap();
        assert_eq!(
            forged
                .validate_update(State::FailedLeft, &original, State::FailedLeft)
                .unwrap_err()
                .code,
            agentchat_core::ErrorCode::IntegrityError
        );
        assert!(!delivery.start_processing(time(3), 10).unwrap());
        assert_eq!(delivery.history, original);
    }
}
