//! Project receipt settings and immutable native-input policy snapshots.

use serde::{Deserialize, Serialize};

use crate::{Error, ErrorCode, Result};

/// Largest supported project delivery-confirmation timeout (thirty days).
pub const MAX_DELIVERY_CONFIRMATION_TIMEOUT_SECONDS: u32 = 2_592_000;

/// Effective policy for subsequently committed messages and prepared inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectReceiptSettings {
    /// Record an automatic read for a confirmed complete message body.
    pub auto_read: bool,
    /// Positive whole-second interval captured at the first processing claim.
    pub delivery_confirmation_timeout_seconds: u32,
}

impl Default for ProjectReceiptSettings {
    fn default() -> Self {
        Self {
            auto_read: true,
            delivery_confirmation_timeout_seconds: 3_600,
        }
    }
}

impl ProjectReceiptSettings {
    /// Reject malformed persisted or caller-constructed settings.
    pub fn validate(self) -> Result<Self> {
        validate_timeout(self.delivery_confirmation_timeout_seconds)?;
        Ok(self)
    }

    /// Freeze only the policy that belongs to an original native input claim.
    /// A later project update cannot reinterpret this copy or its frame digest.
    pub const fn input_policy(self) -> InputReceiptPolicy {
        InputReceiptPolicy {
            auto_read: self.auto_read,
        }
    }
}

/// Partial installation update: omitted and null fields both preserve state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSettingsUpdate {
    /// Supplied false remains an explicit update, distinct from omission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_read: Option<bool>,
    /// A supplied timeout must be within the project-wide positive bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_confirmation_timeout_seconds: Option<u32>,
}

impl ReceiptSettingsUpdate {
    /// Validate before opening a writer or publishing installation intent.
    pub fn validate(self) -> Result<Self> {
        if let Some(seconds) = self.delivery_confirmation_timeout_seconds {
            validate_timeout(seconds)?;
        }
        Ok(self)
    }

    /// Apply one normalized update without changing any omitted field.
    pub fn apply(self, previous: ProjectReceiptSettings) -> Result<ProjectReceiptSettings> {
        self.validate()?;
        ProjectReceiptSettings {
            auto_read: self.auto_read.unwrap_or(previous.auto_read),
            delivery_confirmation_timeout_seconds: self
                .delivery_confirmation_timeout_seconds
                .unwrap_or(previous.delivery_confirmation_timeout_seconds),
        }
        .validate()
    }
}

/// Frozen effective flags stored with an original native claim, never a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputReceiptPolicy {
    /// Acceptance of a complete message body must record its automatic read.
    pub auto_read: bool,
}

/// Original native-frame contract retained with a claim, independent of settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeFrameRevision {
    /// Independent Dispatch identity and typed Notice, without an audit envelope.
    DispatchV4,
}

impl NativeFrameRevision {
    /// Stable private persistence code, not a native host-version allowlist.
    pub const fn code(self) -> i32 {
        match self {
            Self::DispatchV4 => 4,
        }
    }

    /// Refuse an unsupported retained frame rather than reinterpret its bytes.
    pub fn from_code(code: i32) -> Result<Self> {
        match code {
            4 => Ok(Self::DispatchV4),
            _ => Err(Error::new(
                ErrorCode::UnsupportedProtocol,
                "The native frame revision is unsupported; start a fresh project with agentchat install --new-project.",
            )),
        }
    }
}

fn validate_timeout(seconds: u32) -> Result<()> {
    if !(1..=MAX_DELIVERY_CONFIRMATION_TIMEOUT_SECONDS).contains(&seconds) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Delivery confirmation timeout must be 1 through 2592000 seconds.",
        ));
    }
    Ok(())
}
