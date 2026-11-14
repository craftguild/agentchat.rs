//! Keep at most 32 exact native observations for interrupted helper retries.
use super::{
    OwnerNamespace,
    state::{Header, advance},
    unavailable,
};
use crate::claude::{
    NativeCall,
    calls::{identifier, nonce},
};
use agentchat_core::{Error, ErrorCode, Result};
use uuid::Uuid;

fn valid(call: &NativeCall) -> bool {
    // The suffix budget stays independent of the server prefix's spelling.
    let prefix = "mcp__agentchat__";
    nonce(&call.tool_use_id)
        && call.tool_name.starts_with(prefix)
        && call.tool_name.len() > prefix.len()
        && call.tool_name.len() <= prefix.len() + 128
        && identifier(&call.tool_name)
}
pub(super) fn validate(h: &Header) -> Result<()> {
    if h.calls.len() > 32 {
        return Err(unavailable());
    }
    for (index, call) in h.calls.iter().enumerate() {
        if !valid(call)
            || call.observer != h.observer
            || !h.matches(call.root, call.generation)
            || call.sequence == 0
            || call.sequence > h.call_sequence
            || h.calls[..index]
                .iter()
                .any(|c| c.tool_use_id == call.tool_use_id || c.sequence == call.sequence)
        {
            return Err(unavailable());
        }
    }
    Ok(())
}
impl OwnerNamespace {
    /// Capture one exact native tool observation with a durable increasing order.
    /// Identical pending retries reuse that order across helper processes. The
    /// gateway's independent one-use ledger still rejects consumed/revoked IDs.
    /// Call completion/cancellation must release its bounded pending slot.
    pub async fn prepare_call(
        &self,
        root: Uuid,
        generation: u64,
        tool_name: String,
        tool_use_id: String,
    ) -> Result<NativeCall> {
        self.update(move |h| {
            if !h.matches(root, generation) {
                return Err(unavailable());
            }
            if let Some(existing) = h.calls.iter().find(|c| c.tool_use_id == tool_use_id) {
                return if existing.tool_name == tool_name {
                    Ok(existing.clone())
                } else {
                    Err(unavailable())
                };
            }
            if h.calls.len() >= 32 {
                let mut error = Error::new(
                    ErrorCode::Busy,
                    "Native callback observation capacity is full.",
                );
                error.retryable = true;
                return Err(error);
            }
            let call = NativeCall {
                observer: h.observer.clone(),
                root,
                generation,
                sequence: advance(&mut h.call_sequence)?,
                tool_name,
                tool_use_id,
            };
            if !valid(&call) {
                return Err(unavailable());
            }
            h.calls.push(call.clone());
            Ok(call)
        })
        .await
    }
    /// Release the exact completed/cancelled callback slot without changing its
    /// counter floor. An obsolete end callback cannot remove a successor's slot.
    pub async fn forget_call(
        &self,
        root: Uuid,
        generation: u64,
        tool_use_id: String,
    ) -> Result<bool> {
        self.update(move |h| {
            if !h.matches(root, generation) {
                return Ok(false);
            }
            let before = h.calls.len();
            h.calls.retain(|c| c.tool_use_id != tool_use_id);
            Ok(h.calls.len() != before)
        })
        .await
    }
}
