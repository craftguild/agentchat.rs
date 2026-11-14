//! Read-only RPC must not acquire a writer merely to scan native wake work.
mod support;
use agentchat_core::{
    Result,
    application::Mutation,
    delivery::{DeliveryPort, HostReceipt, WakeDispatch},
    model::Context,
    ports::Storage,
    read::Query,
    time::{Clock, Timestamp},
};
use agentchat_protocol::rpc::Client;
use agentchat_runtime::{
    context::ContextRegistry,
    daemon::{RuntimeOptions, Server},
};
use async_trait::async_trait;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{
    Fixture, SyntheticHost,
    runtime::{attach, join, mutate, query, seed},
};

struct ReadinessClock(AtomicUsize);
impl Clock for ReadinessClock {
    fn now(&self) -> Timestamp {
        self.0.fetch_add(1, Ordering::SeqCst);
        Timestamp::parse("2026-10-15T00:00:00.000Z").unwrap()
    }
}
struct UnexpectedNative;
#[async_trait]
impl DeliveryPort for UnexpectedNative {
    async fn present(&self, _: &Context, _: &WakeDispatch) -> Result<HostReceipt> {
        panic!("passive posts and observation must not present native wake work")
    }
}

#[tokio::test]
async fn observation_stays_read_only_while_an_independent_writer_is_reserved() {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let context = ContextRegistry::new(f.manager.clone())
        .issue(seed(&f, "reader"))
        .await
        .unwrap();
    let clock = Arc::new(ReadinessClock(AtomicUsize::new(0)));
    let server = Server::bind(
        f.manager.clone(),
        project.clone(),
        Arc::new(SyntheticHost),
        clock.clone(),
        RuntimeOptions {
            idle: Duration::from_secs(30),
            io_timeout: Duration::from_secs(2),
            housekeeping: Duration::from_secs(10),
        },
    )
    .await
    .unwrap()
    .with_delivery(Arc::new(UnexpectedNative));
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    // Startup maintenance reads this clock once; the immediate first periodic
    // tick reads it again. Observe that tick before reserving the writer. Its
    // transaction finishes before our independent immediate transaction can
    // acquire the lock. The next periodic tick is ten seconds later.
    tokio::time::timeout(Duration::from_secs(2), async {
        while clock.0.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut client = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    join(&mut client, &context, "reader").await;
    attach(&mut client, &context).await;
    mutate(
        &mut client,
        &context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "passive retained body".into(),
        },
    )
    .await;
    // A second read also fences all processing following the prior mutation.
    support::runtime::health(&mut client).await;
    support::runtime::health(&mut client).await;
    let mut unbound = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    unbound.health().await.unwrap();
    let observer = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap()
        .observe()
        .await
        .unwrap();
    let store = f.manager.open(&project).await.unwrap();
    let held = store.begin().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        drop(unbound);
        drop(observer);
        // Closing either unbound connection must not queue a maintenance writer
        // ahead of subsequent reads. Repeated finite exchanges also fence EOF
        // processing while the independent immediate writer remains reserved.
        for _ in 0..4 {
            client.health().await.unwrap();
        }
        let status = support::runtime::health(&mut client).await;
        let history = query(
            &mut client,
            Some(context.context_id),
            Query::History {
                channel: None,
                thread: None,
                page: Default::default(),
            },
        )
        .await;
        assert_eq!(status["statistics"]["messages"], 1);
        assert_eq!(status["statistics"]["read_receipts"], 0);
        assert!(
            status["statistics"]
                .get("acknowledged_deliveries")
                .is_none()
        );
        assert_eq!(history["items"][0]["body"], "passive retained body");
    })
    .await;
    // Release and stop before asserting so the old regression also cleans up
    // its owned server rather than leaving a blocked background coordinator.
    held.commit().await.unwrap();
    drop(client);
    handle.shutdown();
    task.await.unwrap().unwrap();
    Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
    result.expect("two read-only calls must finish without the reserved writer");
}
