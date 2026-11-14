//! Append-only delivery state facts and their transactionally maintained projection.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::{Delivery, DeliveryHistory, DeliveryState};
use crate::{Error, ErrorCode, Result, time::Timestamp};

/// The initial state and longest legal path (pending, processing, timeout, late).
pub const MAX_DELIVERY_TRANSITIONS: usize = 4;

/// One committed state change. Persistence must append this with its projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryTransition {
    /// Stable identity of this committed transition, independent of retries.
    pub id: Uuid,
    /// Required owning recipient obligation.
    pub delivery_id: Uuid,
    /// Consecutive causal order within that obligation, starting at one.
    pub ordinal: u64,
    /// Previous state, absent only for the initial Pending entry.
    pub from_state: Option<DeliveryState>,
    /// State established by this transition.
    pub to_state: DeliveryState,
    /// Actual UTC observation selected by the serialized writer.
    pub changed_at: Timestamp,
    /// Captured inclusive deadline, present only on entry to Processing.
    pub deadline_at: Option<Timestamp>,
}

impl DeliveryTransition {
    /// The initial entry belongs to the same commit as the recipient obligation.
    pub fn initial(delivery: &Delivery) -> Result<Self> {
        delivery.history.validate(delivery.state)?;
        if delivery.state != DeliveryState::Pending {
            return Err(invalid("A delivery history must begin pending."));
        }
        Ok(Self {
            id: Uuid::new_v4(),
            delivery_id: delivery.id,
            ordinal: 1,
            from_state: None,
            to_state: DeliveryState::Pending,
            changed_at: delivery.history.queued_at,
            deadline_at: None,
        })
    }

    /// Plan only actual state changes; attempts and repeated results add no entry.
    /// The caller validates the persisted old chain, then appends and compares its
    /// stored version atomically. UUIDs from rolled-back plans are never published.
    pub fn plan_update(old: &Delivery, new: &Delivery, version: u64) -> Result<Vec<Self>> {
        if old.id != new.id
            || old.dispatch_id != new.dispatch_id
            || old.recipient_id != new.recipient_id
            || !valid_version(old.state, version)
        {
            return Err(invalid(
                "Delivery transition identity or version conflicts.",
            ));
        }
        new.history
            .validate_update(new.state, &old.history, old.state)?;
        if old.state == new.state {
            if old.history != new.history {
                return Err(invalid(
                    "Same-state updates cannot introduce history facts.",
                ));
            }
            return Ok(Vec::new());
        }
        let mut history = old.history.clone();
        let mut previous = old.state;
        let states = if (old.state, new.state)
            == (DeliveryState::Processing, DeliveryState::DeliveredLate)
        {
            vec![DeliveryState::FailedTimeout, DeliveryState::DeliveredLate]
        } else {
            vec![new.state]
        };
        let mut rows = Vec::with_capacity(states.len());
        for state in states {
            let changed_at = required(match state {
                DeliveryState::Processing => new.history.processing_started_at,
                DeliveryState::Delivered | DeliveryState::DeliveredLate => new.history.delivered_at,
                DeliveryState::FailedTimeout => new.history.timed_out_at,
                DeliveryState::FailedLeft => new.history.left_at,
                DeliveryState::Pending => None,
            })?;
            let row = Self {
                id: Uuid::new_v4(),
                delivery_id: new.id,
                ordinal: version
                    + u64::try_from(rows.len()).map_err(|_| invalid("History overflow."))?
                    + 1,
                from_state: Some(previous),
                to_state: state,
                changed_at,
                deadline_at: if state == DeliveryState::Processing {
                    new.history.deadline_at
                } else {
                    None
                },
            };
            row.apply(previous, &mut history)?;
            previous = state;
            rows.push(row);
        }
        if history != new.history || !valid_version(new.state, version + rows.len() as u64) {
            return Err(invalid(
                "Delivery state changes do not explain its first facts.",
            ));
        }
        Ok(rows)
    }

    /// Validate the complete bounded history against the current read projection.
    /// Ordinals, rather than wall-clock or UUID ordering, define causal order.
    pub fn validate_chain(delivery: &Delivery, version: u64, rows: &[Self]) -> Result<()> {
        if rows.is_empty()
            || rows.len() > MAX_DELIVERY_TRANSITIONS
            || version != rows.len() as u64
            || !valid_version(delivery.state, version)
        {
            return Err(invalid(
                "Delivery transition chain is incomplete or oversized.",
            ));
        }
        let first = &rows[0];
        if first.from_state.is_some()
            || first.to_state != DeliveryState::Pending
            || first.deadline_at.is_some()
        {
            return Err(invalid("Delivery history has an invalid initial entry."));
        }
        let mut history = DeliveryHistory::queued(first.changed_at);
        let mut state = DeliveryState::Pending;
        let mut ids = BTreeSet::new();
        for (index, row) in rows.iter().enumerate() {
            if row.id.is_nil()
                || !ids.insert(row.id)
                || row.delivery_id != delivery.id
                || row.ordinal != index as u64 + 1
            {
                return Err(invalid("Delivery transition identity or order is invalid."));
            }
            if index != 0 {
                row.apply(state, &mut history)?;
                state = row.to_state;
            }
        }
        if state != delivery.state || history != delivery.history {
            return Err(invalid(
                "Delivery projection disagrees with its transition history.",
            ));
        }
        history.validate(state)
    }

    fn apply(&self, previous: DeliveryState, history: &mut DeliveryHistory) -> Result<()> {
        use DeliveryState::*;
        if self.from_state != Some(previous)
            || !matches!(
                (previous, self.to_state),
                (Pending, Processing | FailedLeft)
                    | (Processing, Delivered | FailedTimeout | FailedLeft)
                    | (FailedTimeout, DeliveredLate)
            )
            || (self.to_state != Processing && self.deadline_at.is_some())
        {
            return Err(invalid(
                "Delivery transition does not extend the preceding state.",
            ));
        }
        let mut next = history.clone();
        match self.to_state {
            Processing => {
                next.processing_started_at = Some(self.changed_at);
                next.deadline_at = Some(required(self.deadline_at)?);
            }
            Delivered | DeliveredLate => next.delivered_at = Some(self.changed_at),
            FailedTimeout => next.timed_out_at = Some(self.changed_at),
            FailedLeft => next.left_at = Some(self.changed_at),
            Pending => return Err(invalid("A delivery cannot re-enter pending.")),
        }
        next.validate_update(self.to_state, history, previous)?;
        *history = next;
        Ok(())
    }
}

fn valid_version(state: DeliveryState, version: u64) -> bool {
    use DeliveryState::*;
    match state {
        Pending => version == 1,
        Processing => version == 2,
        Delivered | FailedTimeout => version == 3,
        FailedLeft => matches!(version, 2 | 3),
        DeliveredLate => version == 4,
    }
}

fn required(value: Option<Timestamp>) -> Result<Timestamp> {
    value.ok_or_else(|| invalid("Delivery transition is missing a required first fact."))
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::IntegrityError, message)
}
