//! Host lifecycle selectors, distinct from participation and project discovery.

use crate::ConversationKey;
use uuid::Uuid;

/// Fresh native lifecycle before an application context can exist.
/// This nonce is not an application context, agent or native conversation ID.
/// Only authoritative lifecycle callbacks may allocate it or advance its generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapKey {
    /// Fresh opaque nonce for this exact initialization-stage lifecycle.
    pub lifecycle_id: Uuid,
    /// Positive generation in the physical owner's shared lifecycle sequence.
    pub generation: u64,
}

/// One verified call's already-activated immutable lifecycle attachment.
/// Tool arguments cannot select a scope, activate it or promote bootstrap state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallScope {
    /// Exact project context supplied by the native host adapter.
    Conversation(ConversationKey),
    /// Verified native root with no application context or participation.
    Bootstrap(BootstrapKey),
}
impl CallScope {
    pub(crate) fn token_id(self) -> Uuid {
        match self {
            Self::Conversation(key) => key.context_id,
            Self::Bootstrap(key) => key.lifecycle_id,
        }
    }
    pub(crate) fn generation(self) -> u64 {
        match self {
            Self::Conversation(key) => key.generation,
            Self::Bootstrap(key) => key.generation,
        }
    }
}
