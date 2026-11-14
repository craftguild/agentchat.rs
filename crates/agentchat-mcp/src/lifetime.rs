//! A host-owned connection lifetime closes rather than rebinds an SDK session.

use tokio::sync::watch;

/// One-way retirement control supplied by an authoritative host composition.
///
/// Create a fresh controller and server for a fresh connection. Retiring this
/// controller, or dropping its last clone, ends every server subscribed to it;
/// there is no renewal operation. This control neither establishes host identity
/// or presence nor joins/restores an agent. The caller must verify those facts.
#[derive(Clone)]
pub struct ConnectionLifetime {
    retired: watch::Sender<bool>,
}
impl Default for ConnectionLifetime {
    fn default() -> Self {
        Self {
            retired: watch::channel(false).0,
        }
    }
}
impl ConnectionLifetime {
    /// Retire this exact connection permanently, including during initialization.
    /// Already submitted application operations may still commit; their original
    /// semantic request IDs remain the recovery mechanism after a lost result.
    pub fn retire(&self) {
        self.retired.send_replace(true);
    }
    pub(crate) fn is_retired(&self) -> bool {
        *self.retired.borrow()
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.retired.subscribe()
    }
}

impl crate::Server {
    /// A verified same-conversation descriptor replacement may preserve only
    /// an in-flight Join receipt. The group is owned by Conversations, never by
    /// a Server clone: root change, explicit end, close or controller loss still
    /// cancels it. Other operations retain ordinary local retirement fencing.
    pub(super) async fn call_retirement(&self, activating: bool) {
        let local = async {
            self.retirement().await;
            if activating
                && self
                    .activation_replacement
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                std::future::pending::<()>().await;
            }
        };
        let group = async {
            let Some(mut lifetime) = self.activation_group.clone() else {
                return std::future::pending::<()>().await;
            };
            let _ = lifetime.wait_for(|retired| *retired).await;
        };
        tokio::select! { _=local=>{}, _=group=>{} }
    }
    pub(super) fn is_retired(&self) -> bool {
        self.lifetime
            .as_ref()
            .is_some_and(|lifetime| *lifetime.borrow() || lifetime.has_changed().is_err())
    }
    pub(super) async fn retirement(&self) {
        let Some(mut lifetime) = self.lifetime.clone() else {
            return std::future::pending().await;
        };
        // Only the controller owns senders. Its disappearance is loss of
        // authority, never evidence that a retained MCP helper is still live.
        let _ = lifetime.wait_for(|retired| *retired).await;
    }
}
