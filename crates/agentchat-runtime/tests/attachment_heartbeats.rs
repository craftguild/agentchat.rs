//! Owned integration timers refresh only real gate evidence and stop with authority.
mod support;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    model::{Context, Phase},
    ports::{HostGate, HostState},
    read::Query,
};
use agentchat_protocol::operations::{Backend, Target};
use agentchat_protocol::rpc::{Call, success};
use agentchat_runtime::{
    operations::{Launcher, Operations},
    project::Project,
};
use async_trait::async_trait;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{
    SyntheticHost,
    runtime::{Running, join, mutate, options, query},
};
use uuid::Uuid;

struct ObservedHost {
    live: AtomicBool,
    calls: AtomicUsize,
    paused: AtomicBool,
}
impl ObservedHost {
    fn new() -> Self {
        Self {
            live: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
            paused: AtomicBool::new(false),
        }
    }
}
#[async_trait]
impl HostGate for ObservedHost {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.live.load(Ordering::SeqCst) {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Synthetic host authority ended.",
            ));
        }
        let mut state = SyntheticHost.verify(context).await?;
        if self.paused.load(Ordering::SeqCst) {
            state.phase = Phase::Paused;
            state.wake_paused = true;
        }
        Ok(state)
    }
}
struct NoLaunch;
#[async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "The test runtime must already be owned.",
        ))
    }
}
fn backend(r: &Running) -> Operations {
    Operations::new(r.f.manager.clone(), Arc::new(NoLaunch))
}
fn target(r: &Running, context: &Context) -> Target {
    Target {
        actor: None,
        context_id: Some(context.context_id),
        project_path: Some(r.f.workspace.clone()),
        expected_project_id: Some(r.project.identity().id),
        initialization_identity: None,
    }
}

#[tokio::test]
async fn owned_attachment_refreshes_past_the_original_deadline_and_drop_closes_it() {
    let host = Arc::new(ObservedHost::new());
    let r = Running::with_host(options(), None, host.clone()).await;
    let context = r.context("owned-heartbeats").await;
    let mut inspect = r.client().await;
    join(&mut inspect, &context, "alice").await;
    let owned = backend(&r).attach(target(&r, &context)).await.unwrap();
    let before = host.calls.load(Ordering::SeqCst);
    r.clock.advance(30);
    tokio::time::timeout(Duration::from_secs(25), async {
        while host.calls.load(Ordering::SeqCst) == before {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // check waits for the same checked connection's heartbeat reply, making
    // the subsequent manual-clock advance occur after its durable update.
    owned.check().await.unwrap();
    r.clock.advance(30);
    owned.check().await.unwrap();
    assert_eq!(
        query(&mut inspect, Some(context.context_id), Query::Whoami).await["actor"]["connected"],
        true
    );
    drop(owned);
    tokio::time::timeout(Duration::from_secs(5), async {
        while query(&mut inspect, Some(context.context_id), Query::Whoami).await["actor"]["connected"] == true {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("dropping the owner must close its socket despite a retained inspection client");
    r.stop().await;
}

#[tokio::test]
async fn failed_host_evidence_closes_the_owned_socket_before_the_presence_deadline() {
    let host = Arc::new(ObservedHost::new());
    let r = Running::with_host(options(), None, host.clone()).await;
    let context = r.context("authority-loss").await;
    let mut inspect = r.client().await;
    join(&mut inspect, &context, "alice").await;
    let owned = backend(&r).attach(target(&r, &context)).await.unwrap();
    let before = host.calls.load(Ordering::SeqCst);
    r.clock.advance(30);
    host.live.store(false, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let value = query(&mut inspect, Some(context.context_id), Query::Whoami).await;
            if value["actor"]["connected"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(host.calls.load(Ordering::SeqCst) > before);
    assert_eq!(
        owned.check().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    drop(owned);
    r.stop().await;
}

#[tokio::test]
async fn neither_read_only_check_nor_a_late_timer_can_revive_an_expired_incarnation() {
    let host = Arc::new(ObservedHost::new());
    // Keep the owner available for observation after the synthetic presence
    // deadline; the fixture's ordinary 10-second idle exit is a separate gate.
    let mut options = options();
    options.idle = Duration::from_secs(60);
    let r = Running::with_host(options, None, host.clone()).await;
    let context = r.context("expired-incarnation").await;
    let mut inspect = r.client().await;
    join(&mut inspect, &context, "alice").await;
    let owned = backend(&r).attach(target(&r, &context)).await.unwrap();
    let before = host.calls.load(Ordering::SeqCst);
    r.clock.advance(45);
    assert_eq!(
        owned.check().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
    assert_eq!(
        owned.refresh().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
    tokio::time::sleep(Duration::from_secs(16)).await;
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
    assert_eq!(
        query(&mut inspect, Some(context.context_id), Query::Whoami).await["actor"]["connected"],
        false
    );
    drop(owned);
    r.stop().await;
}

#[tokio::test]
async fn a_control_hint_refreshes_warnings_without_rewriting_an_earlier_post_or_marking_read() {
    let host = Arc::new(ObservedHost::new());
    let r = Running::with_host(options(), None, host.clone()).await;
    let a = r.context("hint-author").await;
    let b = r.context("hint-recipient").await;
    let mut alice = r.client().await;
    let mut bob = r.client().await;
    join(&mut alice, &a, "alice").await;
    join(&mut bob, &b, "bob").await;
    let owned = backend(&r).attach(target(&r, &b)).await.unwrap();
    let key = Uuid::new_v4();
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: "@bob pending".into(),
    };
    let original = success(
        alice
            .call(
                Uuid::new_v4(),
                Call::Mutate {
                    request_id: key,
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: a.context_id,
                    },
                    command: command.clone(),
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(original["mention_results"][0]["state"], "queued");
    host.paused.store(true, Ordering::SeqCst);
    owned.check().await.unwrap();
    assert_eq!(
        query(&mut bob, Some(b.context_id), Query::Whoami).await["actor"]["presence"],
        "idle"
    );
    let before = host.calls.load(Ordering::SeqCst);
    owned.refresh().await.unwrap();
    assert_eq!(host.calls.load(Ordering::SeqCst), before + 1);
    let who = query(&mut bob, Some(b.context_id), Query::Whoami).await;
    assert_eq!(who["actor"]["presence"], "paused");
    assert_eq!(who["actor"]["incarnation"], b.incarnation.to_string());
    let next = mutate(
        &mut alice,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob still pending".into(),
        },
    )
    .await;
    assert_eq!(next["mention_results"][0]["state"], "deferred");
    assert_eq!(next["warnings"][0]["code"], "recipient_deferred");
    let replay = success(
        alice
            .call(
                Uuid::new_v4(),
                Call::Mutate {
                    request_id: key,
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: a.context_id,
                    },
                    command,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(replay, original);
    let statistics = support::runtime::health(&mut bob).await;
    assert_eq!(statistics["statistics"]["messages"], 2);
    assert!(
        statistics["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    assert_eq!(statistics["statistics"]["read_receipts"], 0);
    drop(owned);
    r.stop().await;
}
