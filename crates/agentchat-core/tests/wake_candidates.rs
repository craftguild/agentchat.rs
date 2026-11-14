//! Optional storage hints remain conservative and never acquire host authority.
use agentchat_core::{
    Error, ErrorCode, Result,
    application::Application,
    delivery::MAX_WAKE_SCAN_CONTEXTS,
    model::{Context, HostKind},
    ports::{HostGate, HostState, Storage, Transaction},
    time::{Clock, Timestamp},
};
use async_trait::async_trait;
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

struct DefaultStorage(Uuid);
#[async_trait]
impl Storage for DefaultStorage {
    fn project_id(&self) -> Uuid {
        self.0
    }
    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        panic!("hints cannot reserve a writer")
    }
    async fn snapshot(&self) -> Result<Box<dyn agentchat_core::read::Snapshot>> {
        panic!("no snapshot needed")
    }
}
struct FaultyStorage {
    project: Uuid,
    unrelated: bool,
}
#[async_trait]
impl Storage for FaultyStorage {
    fn project_id(&self) -> Uuid {
        self.project
    }
    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        panic!("hints cannot reserve a writer")
    }
    async fn snapshot(&self) -> Result<Box<dyn agentchat_core::read::Snapshot>> {
        panic!("no snapshot needed")
    }
    async fn wake_candidates(&self, _: &[Context], _: Timestamp) -> Result<Vec<Uuid>> {
        if self.unrelated {
            Ok(vec![Uuid::new_v4()])
        } else {
            Err(Error::new(
                ErrorCode::RuntimeUnavailable,
                "Synthetic read failure.",
            ))
        }
    }
}
struct NoAuthority;
#[async_trait]
impl HostGate for NoAuthority {
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::RetainUncertainty,
            ActiveInput::ExactTurnId,
        ))
    }
    async fn verify(&self, _: &Context) -> Result<HostState> {
        panic!("a hint grants no host authority")
    }
}
impl Clock for NoAuthority {
    fn now(&self) -> Timestamp {
        Timestamp::parse("2026-10-15T00:00:00.000Z").unwrap()
    }
}
fn context(project_id: Uuid) -> Context {
    Context {
        project_id,
        context_id: Uuid::new_v4(),
        host: HostKind::Codex,
        host_session_id: "exact-session".into(),
        host_version: "test-only".into(),
        os_profile: "synthetic-test".into(),
        working_directory: "/synthetic".into(),
        incarnation: Uuid::new_v4(),
    }
}
fn app(storage: Arc<dyn Storage>) -> Application {
    Application::new(storage, Arc::new(NoAuthority), Arc::new(NoAuthority))
}

#[tokio::test]
async fn optional_adapter_default_retains_every_context_without_host_or_writer_access() {
    let storage = Arc::new(DefaultStorage(Uuid::new_v4()));
    let app = app(storage.clone());
    let contexts = (0..MAX_WAKE_SCAN_CONTEXTS)
        .map(|_| context(storage.0))
        .collect::<Vec<_>>();
    assert_eq!(
        app.wake_candidates(&contexts).await.unwrap(),
        contexts
            .iter()
            .map(|c| c.context_id)
            .collect::<BTreeSet<_>>()
    );
    assert!(app.wake_candidates(&[]).await.unwrap().is_empty());
    let oversized = vec![contexts[0].clone(); MAX_WAKE_SCAN_CONTEXTS + 1];
    assert_eq!(
        storage
            .wake_candidates(&oversized, NoAuthority.now())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn invalid_descriptors_stay_candidates_and_read_failures_require_caller_fallback() {
    let project = Uuid::new_v4();
    let valid = context(project);
    let mut invalid = valid.clone();
    invalid.context_id = Uuid::new_v4();
    invalid.project_id = Uuid::new_v4();
    for unrelated in [false, true] {
        let app = app(Arc::new(FaultyStorage { project, unrelated }));
        assert_eq!(
            app.wake_candidates(std::slice::from_ref(&invalid))
                .await
                .unwrap(),
            BTreeSet::from([invalid.context_id])
        );
        let error = app
            .wake_candidates(&[invalid.clone(), valid.clone()])
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            if unrelated {
                ErrorCode::IntegrityError
            } else {
                ErrorCode::RuntimeUnavailable
            }
        );
        assert_eq!(
            app.wake_candidates(&vec![valid.clone(); MAX_WAKE_SCAN_CONTEXTS + 1])
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
}
