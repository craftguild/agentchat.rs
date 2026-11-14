//! Real queued RPC bursts share durability without inheriting another actor.
mod support;
use agentchat_core::{
    Result,
    application::{MAX_MUTATION_GROUP, Mutation},
    model::Context,
    ports::{HostGate, HostState},
    read::{PageRequest, Query},
};
use agentchat_runtime::daemon::RuntimeOptions;
use async_trait::async_trait;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use support::{
    SyntheticHost,
    runtime::{Running, join, mutate, query},
};
use tokio::sync::Notify;

#[derive(Default)]
struct BlockOnce {
    armed: AtomicBool,
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl HostGate for BlockOnce {
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
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        SyntheticHost.verify(context).await
    }
}
fn post(body: &str) -> Mutation {
    Mutation::Post {
        channel: Some("dev".into()),
        body: body.into(),
    }
}

#[tokio::test]
async fn queued_concurrent_requests_form_bounded_groups_and_preserve_actor_results() {
    let gate = Arc::new(BlockOnce::default());
    let r = Running::with_host(
        RuntimeOptions {
            idle: Duration::from_secs(30),
            io_timeout: Duration::from_secs(5),
            housekeeping: Duration::from_secs(1),
        },
        None,
        gate.clone(),
    )
    .await;
    let contexts = [r.context("alice").await, r.context("bob").await];
    let mut control = r.client().await;
    join(&mut control, &contexts[0], "alice").await;
    let mut bob = r.client().await;
    join(&mut bob, &contexts[1], "bob").await;
    let mut clients = Vec::new();
    for _ in 0..20 {
        clients.push(r.client().await);
    }
    let before = support::runtime::health(&mut control).await;
    gate.armed.store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let context = contexts[0].clone();
        async move { mutate(&mut control, &context, post("block first")).await }
    });
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for (i, mut client) in clients.into_iter().enumerate() {
        let context = contexts[i % 2].clone();
        tasks.push(tokio::spawn(async move {
            (
                i,
                mutate(&mut client, &context, post(&format!("burst-{i}"))).await,
            )
        }));
    }
    // Keep the first application call suspended while real independent socket
    // readers enqueue the burst. Assertions below require actual grouped work,
    // rather than assuming a delay proves the coordinator collected a group.
    tokio::time::sleep(Duration::from_millis(100)).await;
    gate.release.notify_one();
    first.await.unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for task in tasks {
        let (_, result) = task.await.unwrap();
        assert!(ids.insert(result["message_id"].as_str().unwrap().to_owned()));
    }
    let status = support::runtime::health(&mut bob).await;
    assert_eq!(status["statistics"]["messages"], 21);
    let groups = &status["runtime"]["mutation_groups"];
    assert!(
        groups["batched_groups"].as_u64().unwrap()
            > before["runtime"]["mutation_groups"]["batched_groups"]
                .as_u64()
                .unwrap()
    );
    assert!(groups["largest"].as_u64().unwrap() > 1);
    assert!(groups["largest"].as_u64().unwrap() <= MAX_MUTATION_GROUP as u64);
    let history = query(
        &mut bob,
        None,
        Query::History {
            channel: None,
            thread: None,
            page: PageRequest {
                limit: Some(200),
                after: Default::default(),
            },
        },
    )
    .await;
    for message in history["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["body"].as_str().unwrap().starts_with("burst-"))
    {
        let i = message["body"]
            .as_str()
            .unwrap()
            .strip_prefix("burst-")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert_eq!(
            message["author"]["handle"],
            if i % 2 == 0 { "alice" } else { "bob" }
        );
        assert!(ids.contains(message["id"].as_str().unwrap()));
    }
    r.stop().await;
}
