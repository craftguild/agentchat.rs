//! Release the initial native request before a participation binding exists.
use super::{ProxyEvent, ProxyEvents, required, unavailable};
use agentchat_core::Result;
use tokio::sync::oneshot;

/// One-use pre-entry acknowledgement, retained after an unbound startup release.
/// Replaying that candidate still records its exact fence before managed Join.
pub struct ProxyFence(Option<oneshot::Sender<()>>);
impl ProxyFence {
    pub(super) fn new(sender: oneshot::Sender<()>) -> Self {
        Self(Some(sender))
    }
    /// Acknowledge the exact candidate after fencing any published binding.
    /// An initial request released without a binding has already been acknowledged.
    pub fn send(mut self, value: ()) -> Result<()> {
        self.release(value)
    }
    pub(super) fn release(&mut self, value: ()) -> Result<()> {
        self.0.take().map_or(Ok(()), |sender| {
            sender.send(value).map_err(|_| unavailable())
        })
    }
}

impl ProxyEvents {
    /// Allow one initial request while the original child is current and no
    /// binding has ever been tracked. Preserve the event for the normal Control
    /// consumer: native progress grants no participation or delivery authority.
    /// The supervisor must race this with physical-readiness observation, since
    /// an unloaded thread cannot become ready before its resume reaches native.
    pub async fn prepare_initial(&mut self) -> Result<()> {
        if self.initial_prepared {
            return Err(required());
        }
        let mut event = self.receiver.recv().await.ok_or_else(unavailable)?;
        if let ProxyEvent::Candidate { fence, .. } = &mut event {
            self.observer.release_initial(fence)?;
        }
        self.initial = Some(event);
        self.initial_prepared = true;
        Ok(())
    }
}
