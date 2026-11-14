//! Canonical delivery lifecycle; native authority and stream settlement stay in Core.

use serde::{Deserialize, Serialize};

use crate::{Error, ErrorCode, Result, time::Timestamp};

/// Durable original-recipient outcome, independent of reading or model work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// Queued; time behind a predecessor does not consume the timeout.
    Pending,
    /// Claimed before native I/O, with an immutable start and deadline.
    Processing,
    /// Complete original input acceptance durably confirmed by the deadline.
    Delivered,
    /// Confirmation expired; the original uncertainty fence may still be held.
    FailedTimeout,
    /// The original receiving scope definitively departed.
    FailedLeft,
    /// Original acceptance confirmed after expiry, retaining both first facts.
    DeliveredLate,
}

impl DeliveryState {
    /// Exact shared-wire and relational spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Delivered => "delivered",
            Self::FailedTimeout => "failed_timeout",
            Self::FailedLeft => "failed_left",
            Self::DeliveredLate => "delivered_late",
        }
    }

    /// A terminal result can never be downgraded or resurrected by departure.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Delivered | Self::DeliveredLate | Self::FailedLeft
        )
    }

    /// Legal changes include recording expiry and late acceptance in one commit.
    pub const fn permits(self, next: Self) -> bool {
        use DeliveryState::*;
        matches!(
            (self, next),
            (Pending, Processing | FailedLeft)
                | (
                    Processing,
                    Delivered | FailedTimeout | FailedLeft | DeliveredLate
                )
                | (FailedTimeout, DeliveredLate)
        ) || self as u8 == next as u8
    }
}

impl std::str::FromStr for DeliveryState {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "delivered" => Ok(Self::Delivered),
            "failed_timeout" => Ok(Self::FailedTimeout),
            "failed_left" => Ok(Self::FailedLeft),
            "delivered_late" => Ok(Self::DeliveredLate),
            _ => Err(invalid("Unknown delivery state.")),
        }
    }
}

/// First-fact read projection maintained atomically with transition rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryHistory {
    /// Creation of this original recipient obligation.
    pub queued_at: Timestamp,
    /// First processing claim, before external I/O.
    pub processing_started_at: Option<Timestamp>,
    /// Inclusive deadline fixed by that first claim.
    pub deadline_at: Option<Timestamp>,
    /// Actual expiry observation time, strictly after the deadline.
    pub timed_out_at: Option<Timestamp>,
    /// Actual durable confirmation time of complete original native acceptance.
    pub delivered_at: Option<Timestamp>,
    /// First positive scope departure, absent for completed or timed-out rows.
    pub left_at: Option<Timestamp>,
}

impl DeliveryHistory {
    /// Initial queued facts for a newly committed delivery.
    pub const fn queued(at: Timestamp) -> Self {
        Self {
            queued_at: at,
            processing_started_at: None,
            deadline_at: None,
            timed_out_at: None,
            delivered_at: None,
            left_at: None,
        }
    }

    /// Reject malformed persistence or wire combinations without guessing state.
    pub fn validate(&self, state: DeliveryState) -> Result<()> {
        use DeliveryState::*;
        let mut previous = self.queued_at;
        for observed in [
            self.processing_started_at,
            self.timed_out_at,
            self.delivered_at,
            self.left_at,
        ]
        .into_iter()
        .flatten()
        {
            if observed < previous {
                return Err(invalid("Delivery observation times cannot move backwards."));
            }
            previous = observed;
        }
        let started = match (self.processing_started_at, self.deadline_at) {
            (None, None) => false,
            (Some(start), Some(deadline)) if deadline > start => true,
            _ => return Err(invalid("Delivery start and deadline must be a valid pair.")),
        };
        let expired = self
            .timed_out_at
            .is_some_and(|at| self.deadline_at.is_some_and(|deadline| at > deadline));
        let timely = self
            .delivered_at
            .is_some_and(|at| self.deadline_at.is_some_and(|deadline| at <= deadline));
        let late = self
            .delivered_at
            .is_some_and(|at| self.deadline_at.is_some_and(|deadline| at > deadline));
        let valid = match state {
            Pending => {
                !started
                    && self.delivered_at.is_none()
                    && self.timed_out_at.is_none()
                    && self.left_at.is_none()
            }
            Processing => {
                started
                    && self.delivered_at.is_none()
                    && self.timed_out_at.is_none()
                    && self.left_at.is_none()
            }
            Delivered => started && timely && self.timed_out_at.is_none() && self.left_at.is_none(),
            FailedTimeout => {
                started && expired && self.delivered_at.is_none() && self.left_at.is_none()
            }
            FailedLeft => {
                self.left_at.is_some() && self.delivered_at.is_none() && self.timed_out_at.is_none()
            }
            DeliveredLate => started && expired && late && self.left_at.is_none(),
        };
        if valid {
            Ok(())
        } else {
            Err(invalid("Delivery state and timestamp history disagree."))
        }
    }

    /// Validate one atomic update, preserving every previously committed fact.
    pub fn validate_update(
        &self,
        state: DeliveryState,
        old: &Self,
        previous: DeliveryState,
    ) -> Result<()> {
        old.validate(previous)?;
        self.validate(state)?;
        if !previous.permits(state)
            // Terminal absence is a committed fact too: a pending departure
            // cannot acquire a first processing claim in a later update.
            || (previous.is_terminal() && self != old)
            || self.queued_at != old.queued_at
            || [
                (old.processing_started_at, self.processing_started_at),
                (old.deadline_at, self.deadline_at),
                (old.timed_out_at, self.timed_out_at),
                (old.delivered_at, self.delivered_at),
                (old.left_at, self.left_at),
            ]
            .into_iter()
            .any(|(before, after)| before.is_some() && before != after)
        {
            return Err(invalid(
                "Delivery transition or first timestamp is immutable.",
            ));
        }
        Ok(())
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::IntegrityError, message)
}

impl super::Delivery {
    /// Lifecycle permission for a new submission, independent of native authority.
    /// Receipt-only recovery of an already submitted input does not use this gate.
    pub fn within_processing_deadline(&self, now: Timestamp) -> bool {
        self.state == DeliveryState::Processing
            && self
                .history
                .deadline_at
                .is_some_and(|deadline| now <= deadline)
    }

    /// Claim once before native I/O; retries cannot extend the original deadline.
    pub fn start_processing(&mut self, now: Timestamp, timeout_seconds: u32) -> Result<bool> {
        self.history.validate(self.state)?;
        if self.state != DeliveryState::Pending {
            return Ok(false);
        }
        if !(1..=crate::project::MAX_DELIVERY_CONFIRMATION_TIMEOUT_SECONDS)
            .contains(&timeout_seconds)
        {
            return Err(invalid("Invalid processing timeout."));
        }
        let mut history = self.history.clone();
        history.processing_started_at = Some(now);
        history.deadline_at = Some(now.after_seconds(i64::from(timeout_seconds))?);
        self.transition(DeliveryState::Processing, history)
    }

    /// Materialize expiry without implying rejection or releasing an unknown head.
    pub fn expire(&mut self, now: Timestamp) -> Result<bool> {
        self.history.validate(self.state)?;
        if self.state != DeliveryState::Processing
            || self
                .history
                .deadline_at
                .is_none_or(|deadline| now <= deadline)
        {
            return Ok(false);
        }
        let mut history = self.history.clone();
        history.timed_out_at = Some(now);
        self.transition(DeliveryState::FailedTimeout, history)
    }

    /// Record only correlated complete acceptance; delayed maintenance loses no fact.
    /// The caller must settle its exact stream head in the same FULL commit.
    pub fn confirm_delivery(&mut self, now: Timestamp) -> Result<bool> {
        self.history.validate(self.state)?;
        if matches!(
            self.state,
            DeliveryState::Delivered | DeliveryState::DeliveredLate
        ) {
            return Ok(false);
        }
        if !matches!(
            self.state,
            DeliveryState::Processing | DeliveryState::FailedTimeout
        ) {
            return Err(invalid("Only an original processing attempt can complete."));
        }
        let mut history = self.history.clone();
        history.delivered_at = Some(now);
        let next = if now <= history.deadline_at.expect("validated start") {
            DeliveryState::Delivered
        } else {
            history.timed_out_at.get_or_insert(now);
            DeliveryState::DeliveredLate
        };
        self.transition(next, history)
    }

    /// End unfinished scope work; an earlier timeout and every terminal fact survive.
    pub fn depart(&mut self, now: Timestamp) -> Result<bool> {
        self.history.validate(self.state)?;
        if !matches!(
            self.state,
            DeliveryState::Pending | DeliveryState::Processing
        ) {
            return Ok(false);
        }
        let mut history = self.history.clone();
        history.left_at = Some(now);
        self.transition(DeliveryState::FailedLeft, history)
    }

    fn transition(&mut self, next: DeliveryState, history: DeliveryHistory) -> Result<bool> {
        history.validate_update(next, &self.history, self.state)?;
        self.state = next;
        self.history = history;
        Ok(true)
    }
}
