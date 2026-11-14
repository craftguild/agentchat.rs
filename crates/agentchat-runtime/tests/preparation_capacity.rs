//! Failed preparation roots cannot monopolize the bounded native pipeline.

#[path = "../src/daemon/delivery.rs"]
#[allow(dead_code)]
mod dispatch;
mod support;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::{Application, Mutation},
    delivery::{DeliveryPort, HostReceipt, WakeDispatch},
    model::Context,
    ports::{HostGate, HostState},
};
use agentchat_runtime::context::ContextRegistry;
use agentchat_runtime::diagnostics::{self, RuntimeLog};
use async_trait::async_trait;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::{
    Fixture, SyntheticHost,
    runtime::{ManualClock, seed},
};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Default)]
struct Gate(Mutex<HashSet<Uuid>>);
#[async_trait]
impl HostGate for Gate {
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
    fn allows_unmanaged_participation(&self) -> bool {
        true
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        if self.0.lock().unwrap().contains(&context.context_id) {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "Synthetic rejected root.",
            ));
        }
        SyntheticHost.verify(context).await
    }
}
struct Port(mpsc::Sender<WakeDispatch>);
#[async_trait]
impl DeliveryPort for Port {
    async fn present(&self, _: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        self.0.send(dispatch.clone()).await.unwrap();
        Ok(HostReceipt::default())
    }
}

#[tokio::test]
async fn thirty_two_failed_preparations_cannot_starve_the_next_exact_recipient() {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let store = f.manager.open(&project).await.unwrap();
    let gate = Arc::new(Gate::default());
    let app = Arc::new(Application::new(
        store.clone(),
        Arc::new(ManualClock::default()),
        gate.clone(),
    ));
    let registry = ContextRegistry::new(f.manager.clone());
    let mut contexts = Vec::new();
    for index in 0..33 {
        let name = format!("agent{index}");
        let context = registry.issue(seed(&f, &name)).await.unwrap();
        app.execute(
            Uuid::new_v4(),
            &context,
            Mutation::Join { name: Some(name) },
        )
        .await
        .unwrap();
        contexts.push(context);
    }
    contexts.sort_by_key(|context| context.context_id);
    app.execute(
        Uuid::new_v4(),
        &contexts[0],
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@all preserve independent progress".into(),
        },
    )
    .await
    .unwrap();
    gate.0
        .lock()
        .unwrap()
        .extend(contexts[..32].iter().map(|c| c.context_id));
    let (send, mut receive) = mpsc::channel(1);
    let diagnostics = RuntimeLog::open(project.data_dir(), contexts[0].project_id, Uuid::new_v4())
        .await
        .unwrap();
    let mut dispatcher = dispatch::Dispatches::new(
        Some(dispatch::DispatchPort::Hint(Arc::new(Port(send)))),
        diagnostics.sink(),
    );
    // A >32-context advisory scan fails closed to ordinary per-root preparation.
    assert_eq!(
        dispatcher
            .start(&app, contexts.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(dispatcher.status().jobs, 32);
    tokio::time::timeout(Duration::from_secs(3), async {
        while dispatcher.status().jobs != 0 {
            let outcome = dispatcher.receiver.recv().await.unwrap();
            let (finished, failure, completed_job) = dispatcher.take_ready(Some(outcome), 8);
            assert!(completed_job);
            assert!(finished.is_empty());
            assert_eq!(failure.unwrap().code, ErrorCode::UnsupportedHost);
        }
    })
    .await
    .unwrap();
    assert!(receive.try_recv().is_err());
    assert_eq!(
        dispatcher
            .start(&app, contexts.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert!(dispatcher.status().jobs <= 32);
    let native = tokio::time::timeout(Duration::from_secs(3), receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(native.hint.context_id, contexts[32].context_id);
    assert_eq!(native.attempt, 1);
    tokio::time::timeout(Duration::from_secs(3), async {
        while dispatcher.status().jobs != 0 {
            let outcome = dispatcher.receiver.recv().await.unwrap();
            let (_, failure, completed_job) = dispatcher.take_ready(Some(outcome), 8);
            assert!(completed_job);
            if let Some(error) = failure {
                assert_eq!(error.code, ErrorCode::UnsupportedHost);
            }
        }
    })
    .await
    .unwrap();
    drop(dispatcher);
    diagnostics.close().await.unwrap();
    drop(app);
    Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
}
