//! Short-lived evidence distinguishes the pending head from accepted native rows.
use super::required;
use agentchat_core::Result;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Permit {
    pub root: Uuid,
    pub generation: u64,
    pub context_id: Uuid,
    pub incarnation: Uuid,
    pub token: Uuid,
    epoch: u64,
    issued: Option<u64>,
    queue: Option<Queue>,
    head: Option<Frame>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    digest: [u8; 32],
    bytes: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Queue {
    id: Uuid,
    digest: [u8; 32],
    bytes: usize,
}
impl Permit {
    pub(super) fn valid(&self, input: &Value, final_hook: bool) -> Result<()> {
        if input["session_id"] != self.root.to_string()
            || [self.root, self.context_id, self.incarnation, self.token]
                .into_iter()
                .any(|id| id.is_nil())
            || self.generation == 0
            || self.generation > 9_007_199_254_740_991
            || self.epoch > 9_007_199_254_740_991
        {
            return Err(required());
        }
        if !final_hook {
            return if self.queue.is_none() && self.head.is_none() && self.issued.is_none() {
                Ok(())
            } else {
                Err(required())
            };
        }
        let now: u64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| required())?
            .as_millis()
            .try_into()
            .map_err(|_| required())?;
        if self
            .issued
            .is_none_or(|issued| issued > now || now - issued >= 2000)
            || self.queue.is_some() == self.head.is_some()
            || self.queue.as_ref().is_some_and(|q| q.id.is_nil())
            || self
                .frame()
                .is_none_or(|(_, bytes)| bytes == 0 || bytes > 65_536 + 8192)
        {
            return Err(required());
        }
        Ok(())
    }
    pub(super) fn pending_proof(&self) -> Option<([u8; 32], usize)> {
        self.head.as_ref().map(|p| (p.digest, p.bytes))
    }
    fn frame(&self) -> Option<([u8; 32], usize)> {
        match (&self.queue, &self.head) {
            (Some(q), None) => Some((q.digest, q.bytes)),
            (None, Some(p)) => Some((p.digest, p.bytes)),
            _ => None,
        }
    }
}
pub(super) fn frames(text: &str, permits: &[Permit]) -> Result<()> {
    let mut ranges = Vec::new();
    let mut ids = BTreeSet::new();
    for p in permits {
        let (digest, bytes) = p.frame().ok_or_else(required)?;
        let identity = p.queue.as_ref().map_or((false, p.token), |q| (true, q.id));
        if !ids.insert(identity) {
            return Err(required());
        }
        let prefix = format!(
            "AGENTCHAT_INPUT_NOTICE root={} context={} token={}\n",
            p.root, p.context_id, p.token
        );
        let mut matches = text.match_indices(&prefix).filter_map(|(start, _)| {
            let end = start.checked_add(bytes)?;
            let frame = text.get(start..end)?;
            (<[u8; 32]>::from(Sha256::digest(frame.as_bytes())) == digest).then_some((start, end))
        });
        let range = matches.next().ok_or_else(required)?;
        if matches.next().is_some()
            || ranges
                .iter()
                .any(|&(start, end)| range.0 < end && start < range.1)
        {
            return Err(required());
        }
        ranges.push(range);
    }
    // A marker lookalike inside an original body is harmless. An extra frame
    // outside the proved ranges cannot piggyback on another input's permit.
    if text
        .match_indices("AGENTCHAT_INPUT_NOTICE root=")
        .any(|(at, _)| !ranges.iter().any(|&(start, end)| at >= start && at < end))
    {
        return Err(required());
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../../tests/unit/input_permits.rs"]
mod tests;
