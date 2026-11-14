//! A native hook must not wait behind a command awaiting that same native host.
mod support;
use agentchat_core::{
    Result,
    application::{Application, Mutation},
    model::{Context, Phase},
    ports::{HostGate, HostState},
};
use agentchat_protocol::rpc::{Call, success};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use support::runtime::{Running, join, mutate, options};
use tokio::sync::Notify;
use uuid::Uuid;
#[derive(Default)]
struct Host {
    blocked: AtomicBool,
    entered: Notify,
    release: Notify,
}
#[async_trait::async_trait]
impl HostGate for Host {
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
    async fn verify(&self, _: &Context) -> Result<HostState> {
        if self.blocked.load(Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(HostState {
            phase: Phase::Idle,
            active_turn_id: None,
            wake_paused: false,
        })
    }
}
#[tokio::test]
async fn native_claim_check_progresses_while_the_coordinator_awaits_native_verification() {
    let host = Arc::new(Host::default());
    let r = Running::with_host(options(), None, host.clone()).await;
    let a = r.context("sender").await;
    let b = r.context("recipient").await;
    let mut sender = r.client().await;
    let mut recipient = r.client().await;
    let mut checker = r.client().await;
    join(&mut sender, &a, "alice").await;
    join(&mut recipient, &b, "bob").await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob exact claim".into(),
        },
    )
    .await;
    let app = Application::new(
        r.f.manager.open(&r.project).await.unwrap(),
        r.clock.clone(),
        host.clone(),
    );
    let claim = app.prepare_wake(&b).await.unwrap().unwrap();
    host.blocked.store(true, Ordering::SeqCst);
    let id = b.context_id;
    let waiting = tokio::spawn(async move {
        recipient
            .call(Uuid::new_v4(), Call::Heartbeat { context_id: id })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), host.entered.notified())
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        checker.call(
            Uuid::new_v4(),
            Call::CheckDelivery {
                context_id: id,
                batch_id: claim.hint.batch_id,
                token: claim.token,
                delivery_ids: claim
                    .hint
                    .events
                    .iter()
                    .map(|event| event.delivery_id)
                    .collect(),
            },
        ),
    )
    .await;
    let native_result = tokio::time::timeout(
        Duration::from_secs(1),
        checker.call(
            Uuid::new_v4(),
            Call::CheckNative {
                context_id: id,
                generation: Uuid::new_v4(),
                delivery_id: Uuid::new_v4(),
                token: Uuid::new_v4(),
            },
        ),
    )
    .await;
    // Release the exact synthetic native waiter before asserting, including on
    // the pre-fix failure, so no delayed task can conceal the circular wait.
    host.blocked.store(false, Ordering::SeqCst);
    host.release.notify_one();
    waiting.await.unwrap().unwrap();
    let value = success(
        result
            .expect("read-only hook fence waited behind native verification")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["current"], true);
    assert_eq!(
        success(
            native_result
                .expect("full-native fence waited behind native verification")
                .unwrap(),
        )
        .unwrap()["current"],
        false,
        "a nonmatching full-native claim is refused without waiting on the host"
    );
    assert_eq!(
        checker
            .query(Uuid::new_v4(), None, agentchat_core::read::Query::Whoami)
            .await
            .unwrap_err()
            .code,
        agentchat_core::ErrorCode::SessionRequired
    );
    assert_eq!(
        checker.health().await.unwrap().application.project_id,
        r.project.identity().id
    );
    r.stop().await;
}
