//! Trusted mechanism descriptions, independent of host identity and permission.
use crate::{Error, ErrorCode, Result};

/// Recovery available for an original, durably claimed native attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRecovery {
    /// Read the original correlated result without publishing or signalling again.
    OriginalReceipt,
    /// Retain uncertainty until the original caller supplies a definite outcome.
    RetainUncertainty,
}

/// Whether the native active-input mechanism requires an exact turn reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveInput {
    /// The mechanism itself needs no turn ID; current controls still apply.
    WithoutTurnId,
    /// An absent turn reference cannot authorize this mechanism.
    ExactTurnId,
}

/// Adapter configuration captured before opening a database writer.
/// These facts grant no identity, liveness, compatibility or control authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputCapabilities {
    /// Complete input with recipient-relative attention is implemented.
    pub addressed: bool,
    /// Complete passive history input is implemented without an inferred task.
    pub passive: bool,
    /// Active complete-input mechanism requirement.
    pub active_input: ActiveInput,
    /// Separate hint mechanism requirement; never applied to full input.
    pub active_hint: ActiveInput,
    /// Original-attempt recovery mechanism, retained in the committed head.
    pub recovery: InputRecovery,
}
impl InputCapabilities {
    /// Describe complete addressed/passive input without expected-turn steering.
    pub const fn complete(recovery: InputRecovery, active_hint: ActiveInput) -> Self {
        Self {
            addressed: true,
            passive: true,
            active_input: ActiveInput::WithoutTurnId,
            active_hint,
            recovery,
        }
    }

    /// Reject a missing route before reserving any native attempt.
    pub fn require_input(self, addressed: bool) -> Result<()> {
        if if addressed {
            self.addressed
        } else {
            self.passive
        } {
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::UnsupportedProtocol,
                "The adapter lacks the required complete native input mechanism.",
            ))
        }
    }
}
