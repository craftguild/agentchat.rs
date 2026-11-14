//! Installed native state and transport with live API-surface checks.
mod input;
mod presentation;
use super::{
    observer::{NativeObserver, NativeProfile},
    process::ProcessIdentity,
    routing::{self, Route},
};
use agentchat_core::{Error, ErrorCode, Result, model::Context, ports::HostState};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Shared native implementation for the managed daemon and isolated host tests.
/// The selected version prevents instance mixing; the installed module checks
/// its required host API surface and reports any missing pointer to the caller.
pub struct NativeAdapter {
    home: PathBuf,
    observers: Mutex<HashMap<Uuid, Observer>>,
}
struct Observer {
    owner: ProcessIdentity,
    profile: NativeProfile,
    observer: Arc<NativeObserver>,
}
impl NativeAdapter {
    /// Declare original-result recovery and full input without native I/O.
    pub fn input_capabilities(&self) -> agentchat_core::delivery::InputCapabilities {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        InputCapabilities::complete(InputRecovery::OriginalReceipt, ActiveInput::WithoutTurnId)
    }
    /// Capture a state root without discovery, files, host processes or timers.
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            observers: Mutex::default(),
        }
    }
    /// Check the exact current registered cursor, transport and physical owner
    /// without challenging the host, refreshing phase evidence or opening an
    /// observer. Public actor lookup uses this read-only association check;
    /// mutations still obtain their own fresh shared HostGate authorization.
    pub async fn check_context(&self, context: &Context) -> Result<()> {
        routing::resolve(&self.home, context).await?.check().await
    }
    /// Obtain a fresh exact-context native state without joining or renewing it.
    /// Read-only cursor/endpoint checks bracket the native challenge. A registry
    /// descriptor, surviving helper or cached phase cannot replace that proof.
    pub async fn observe(&self, context: &Context) -> Result<HostState> {
        let route = routing::resolve(&self.home, context).await?;
        self.observe_route(&route).await
    }
    /// Challenge the exact process the caller actually launched, in addition to
    /// the immutable native context. Other same-version CLIs cannot substitute
    /// their fresh answer. Startup/readiness and admission remain separate.
    pub async fn observe_owned(
        &self,
        context: &Context,
        owner: &ProcessIdentity,
    ) -> Result<HostState> {
        let route = routing::resolve(&self.home, context).await?;
        if route.native.owner != *owner {
            return Err(required());
        }
        self.observe_route(&route).await
    }
    async fn observe_route(&self, route: &Route) -> Result<HostState> {
        route.check().await?;
        let observer = self.observer(route).await?;
        let state = observer.verify(&route.native.registration.context).await?;
        route.check().await?;
        Ok(state)
    }
    async fn observer(&self, route: &Route) -> Result<Arc<NativeObserver>> {
        let id = route.namespace.id();
        {
            let cache = self.observers.lock().await;
            if let Some(entry) = cache.get(&id) {
                return entry.get(route);
            }
        }
        // No cache lock spans native or filesystem I/O. A racing constructor
        // retains the first window; it cannot reset a live owner's replay floor.
        let observer = Arc::new(
            NativeObserver::open_owned(
                route.namespace.directory().into(),
                route.profile.clone(),
                &route.native.owner,
            )
            .await?,
        );
        let mut cache = self.observers.lock().await;
        if let Some(entry) = cache.get(&id) {
            return entry.get(route);
        }
        // Uncertain owners keep their replay window and capacity reservation.
        // Only positive original-process loss may evict that continuity state.
        cache.retain(|_, entry| {
            entry.owner.continuity_state() != super::process::ContinuityState::Gone
        });
        if cache.len() >= 32 {
            let mut error = Error::new(ErrorCode::Busy, "Native adapter owner capacity is full.");
            error.retryable = true;
            return Err(error);
        }
        cache.insert(
            id,
            Observer {
                owner: route.native.owner.clone(),
                profile: route.profile.clone(),
                observer: observer.clone(),
            },
        );
        Ok(observer)
    }
}
impl Observer {
    fn get(&self, route: &Route) -> Result<Arc<NativeObserver>> {
        if self.owner != route.native.owner
            || self.profile.version != route.profile.version
            || self.profile.executable != route.profile.executable
        {
            return Err(required());
        }
        Ok(self.observer.clone())
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The exact installed native boundary is unavailable.",
    )
}
