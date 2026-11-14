//! Revalidate a previously captured owner without reopening its engine image.
use super::{ProcessIdentity, parent_pid, unavailable};
use agentchat_core::Result;

impl ProcessIdentity {
    /// Verify this retained original owner is still the helper's direct parent.
    /// Initial image admission must already have captured this exact token.
    /// This check grants no conversation, namespace or native permission.
    pub fn verify_parent(&self) -> Result<()> {
        if parent_pid(std::process::id())? != self.pid || !self.is_continuing() {
            return Err(unavailable());
        }
        if parent_pid(std::process::id())? != self.pid {
            return Err(unavailable());
        }
        Ok(())
    }

    /// Verify the bounded live ancestry to an already admitted original token.
    /// A replacement engine at the same path cannot replace this owner. The
    /// original executable may have been removed by its normal updater.
    pub fn verify_ancestor(&self) -> Result<()> {
        let mut chain = vec![Self::capture(std::process::id())?];
        for _ in 0..8 {
            let child = chain.last().ok_or_else(unavailable)?;
            let pid = parent_pid(child.pid)?;
            if !child.is_continuing() {
                return Err(unavailable());
            }
            if pid == self.pid {
                if !self.is_continuing() || parent_pid(child.pid)? != self.pid {
                    return Err(unavailable());
                }
                for pair in chain.windows(2) {
                    if !pair[0].is_continuing()
                        || !pair[1].is_continuing()
                        || parent_pid(pair[0].pid)? != pair[1].pid
                    {
                        return Err(unavailable());
                    }
                }
                return Ok(());
            }
            chain.push(Self::capture(pid)?);
        }
        Err(unavailable())
    }
}
