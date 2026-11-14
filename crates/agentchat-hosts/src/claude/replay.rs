//! A bounded native observation window survives consumption and root switches.

use agentchat_core::{Error, ErrorCode, Result};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Replay {
    highest: u64,
    observations: BTreeMap<u64, [u8; 32]>,
}
impl Replay {
    pub fn record(&mut self, sequence: u64, nonce: [u8; 32]) -> Result<()> {
        let highest = self.highest.max(sequence);
        let floor = highest.saturating_sub(4095).max(1);
        if sequence == 0
            || sequence < floor
            || self.observations.contains_key(&sequence)
            || self.observations.values().any(|old| old == &nonce)
        {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Native call observation was replayed or is too old.",
            ));
        }
        self.observations.retain(|sequence, _| *sequence >= floor);
        self.observations.insert(sequence, nonce);
        self.highest = highest;
        Ok(())
    }
}
