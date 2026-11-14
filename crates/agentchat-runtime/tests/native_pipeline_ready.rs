//! Real owner-side queue tests; no real host, profile or native process is used.
mod support;
use agentchat_core::{
    Result,
    application::Mutation,
    delivery::{HostReceipt, MessageProgress},
    model::{Context, DeliveryState, LaunchIdentity, LaunchOwnerState},
    ports::{HostGate, HostState, Storage},
};
use agentchat_protocol::rpc::{Call, success};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use support::{
    native_host::NativeHost,
    runtime::{Running, health, mutate, options},
};
use tokio::sync::Notify;
use uuid::Uuid;
#[derive(Default)]
struct Host {
    inner: NativeHost,
    target: Mutex<Option<Uuid>>,
    entered: Notify,
    release: Notify,
}
#[async_trait::async_trait]
impl HostGate for Host {
    fn input_capabilities(
        &self,
        c: &Context,
    ) -> Result<agentchat_core::delivery::InputCapabilities> {
        self.inner.input_capabilities(c)
    }
    async fn launch_identity(&self, c: &Context) -> Result<Option<LaunchIdentity>> {
        self.inner.launch_identity(c).await
    }
    fn launch_owner(&self, i: &LaunchIdentity) -> LaunchOwnerState {
        self.inner.launch_owner(i)
    }
    async fn verify(&self, c: &Context) -> Result<HostState> {
        let held = {
            let mut target = self.target.lock().unwrap();
            if *target == Some(c.context_id) {
                target.take();
                true
            } else {
                false
            }
        };
        if held {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.verify(c).await
    }
}
async fn start(r: &Running, h: &Host, name: &str) -> (agentchat_protocol::rpc::Client, Context) {
    let c = r.context(name).await;
    h.inner.bind(&c);
    let mut client = r.client().await;
    success(
        client
            .call(
                Uuid::new_v4(),
                Call::StartLaunch {
                    context_id: c.context_id,
                    name: Some(name.into()),
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    (client, c)
}
async fn drain(r: &Running, c: &Context) {
    while let Some(d) = r.pipeline.prepare_native(c).await.unwrap() {
        assert!(
            r.pipeline
                .finish_native(c, &d, Ok(MessageProgress::Accepted(HostReceipt::default())))
                .await
                .unwrap()
        );
    }
}
async fn hold(r: &Running, h: &Host, c: &Context) -> tokio::task::JoinHandle<()> {
    *h.target.lock().unwrap() = Some(c.context_id);
    let mut client = r.client().await;
    let id = c.context_id;
    let task = tokio::spawn(async move {
        success(
            client
                .call(Uuid::new_v4(), Call::Heartbeat { context_id: id })
                .await
                .unwrap(),
        )
        .unwrap();
    });
    tokio::time::timeout(Duration::from_secs(2), h.entered.notified())
        .await
        .unwrap();
    task
}
#[tokio::test(flavor = "current_thread")]
async fn ready_preparations_group_and_cancelled_prep_does_not_create_a_head() {
    let h = Arc::new(Host::default());
    let r = Running::with_host(options(), None, h.clone()).await;
    let (mut sender, a) = start(&r, &h, "author").await;
    let mut contexts = Vec::new();
    let mut clients = Vec::new();
    for n in 0..4 {
        let (client, c) = start(&r, &h, &format!("receiver{n}")).await;
        clients.push(client);
        contexts.push(c);
    }
    for c in &contexts {
        drain(&r, c).await;
    }
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@all grouped original".into(),
        },
    )
    .await;
    let barrier = hold(&r, &h, &a).await;
    let mut jobs = Vec::new();
    for c in contexts.clone() {
        let pipeline = r.pipeline.clone();
        jobs.push(tokio::spawn(
            async move { pipeline.prepare_native(&c).await },
        ));
    }
    // Each task reaches the same non-full bounded ingress on this executor.
    tokio::task::yield_now().await;
    let cancelled = jobs.pop().unwrap();
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    let pipeline = r.pipeline.clone();
    let context = contexts[0].clone();
    let duplicate = tokio::spawn(async move { pipeline.prepare_native(&context).await });
    tokio::task::yield_now().await;
    h.release.notify_one();
    barrier.await.unwrap();
    let mut outcomes = Vec::new();
    for job in jobs {
        outcomes.push(job.await.unwrap().unwrap());
    }
    let duplicate = duplicate.await.unwrap().unwrap();
    assert_eq!(
        usize::from(outcomes[0].is_some()) + usize::from(duplicate.is_some()),
        1
    );
    outcomes[0] = outcomes[0].take().or(duplicate);
    let dispatches = outcomes.into_iter().map(Option::unwrap).collect::<Vec<_>>();
    let status = health(&mut sender).await;
    assert!(
        status["runtime"]["mutation_groups"]["largest_commit_group"]
            .as_u64()
            .unwrap()
            >= 2,
        "{status}"
    );
    let store = r.f.manager.open(&r.project).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let cancelled = &contexts[3];
    let agent = tx
        .conversation(cancelled.host, &cancelled.host_session_id)
        .await
        .unwrap()
        .unwrap();
    let generation = tx
        .latest_launch(agent.id)
        .await
        .unwrap()
        .unwrap()
        .identity
        .generation;
    assert!(
        tx.message_stream(generation)
            .await
            .unwrap()
            .unwrap()
            .head
            .is_none()
    );
    let cursor = tx
        .message_stream(generation)
        .await
        .unwrap()
        .unwrap()
        .settled_seq;
    assert_eq!(
        tx.next_native(agent.id, cursor)
            .await
            .unwrap()
            .unwrap()
            .delivery()
            .state,
        DeliveryState::Pending
    );
    tx.rollback().await.unwrap();
    for (c, d) in contexts.iter().zip(&dispatches) {
        assert!(
            r.pipeline
                .finish_native(c, d, Ok(MessageProgress::Accepted(HostReceipt::default())))
                .await
                .unwrap()
        );
        assert!(
            !r.pipeline
                .finish_native(c, d, Ok(MessageProgress::Accepted(HostReceipt::default())))
                .await
                .unwrap()
        );
    }
    for c in &contexts {
        drain(&r, c).await;
    }
    drop(sender);
    r.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn closed_positive_completion_is_committed_and_unknown_keeps_original_token() {
    let h = Arc::new(Host::default());
    let r = Running::with_host(options(), None, h.clone()).await;
    let (mut sender, a) = start(&r, &h, "author").await;
    let (_receiver, b) = start(&r, &h, "recipient").await;
    drain(&r, &b).await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@recipient original".into(),
        },
    )
    .await;
    let dispatch = r.pipeline.prepare_native(&b).await.unwrap().unwrap();
    // A lost result and elapsed time do not clear or replace the original token.
    r.clock.advance(6);
    assert!(r.pipeline.prepare_native(&b).await.unwrap().is_none());
    let barrier = hold(&r, &h, &a).await;
    let pipeline = r.pipeline.clone();
    let context = b.clone();
    let original = dispatch.clone();
    let completion = tokio::spawn(async move {
        pipeline
            .finish_native(
                &context,
                &original,
                Ok(MessageProgress::Accepted(HostReceipt::default())),
            )
            .await
    });
    tokio::task::yield_now().await;
    completion.abort();
    assert!(completion.await.unwrap_err().is_cancelled());
    h.release.notify_one();
    barrier.await.unwrap();
    // This ordered RPC is a real queue fence behind the dropped callback.
    let _ = health(&mut sender).await;
    let store = r.f.manager.open(&r.project).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let row = tx.delivery(dispatch.delivery_id()).await.unwrap().unwrap();
    assert_eq!(row.state, DeliveryState::Delivered);
    assert_eq!(row.attempts, 1);
    tx.rollback().await.unwrap();
    assert!(
        !r.pipeline
            .finish_native(
                &b,
                &dispatch,
                Ok(MessageProgress::Accepted(HostReceipt::default()))
            )
            .await
            .unwrap()
    );
    drop(sender);
    r.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn heavy_native_observation_sees_processing_and_does_not_hold_the_rpc_writer() {
    let h = Arc::new(Host::default());
    let r = Running::with_host(options(), None, h.clone()).await;
    let (mut sender, a) = start(&r, &h, "author").await;
    let (_receiver, b) = start(&r, &h, "recipient").await;
    drain(&r, &b).await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@recipient original".into(),
        },
    )
    .await;
    *h.target.lock().unwrap() = Some(b.context_id);
    let pipeline = r.pipeline.clone();
    let context = b.clone();
    let prepare = tokio::spawn(async move { pipeline.prepare_native(&context).await });
    tokio::time::timeout(Duration::from_secs(2), h.entered.notified())
        .await
        .unwrap();
    let store = r.f.manager.open(&r.project).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let agent = tx
        .conversation(b.host, &b.host_session_id)
        .await
        .unwrap()
        .unwrap();
    let generation = tx
        .latest_launch(agent.id)
        .await
        .unwrap()
        .unwrap()
        .identity
        .generation;
    let stream = tx.message_stream(generation).await.unwrap().unwrap();
    assert!(stream.head.is_none());
    let item = tx
        .next_native(agent.id, stream.settled_seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.delivery().state, DeliveryState::Processing);
    tx.rollback().await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        mutate(
            &mut sender,
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "RPC writer continued during native observation".into(),
            },
        ),
    )
    .await
    .unwrap();
    h.release.notify_one();
    let dispatch = prepare.await.unwrap().unwrap().unwrap();
    let mut tx = store.begin().await.unwrap();
    let head = tx
        .message_stream(dispatch.generation())
        .await
        .unwrap()
        .unwrap()
        .head
        .unwrap();
    assert_eq!(head.token, dispatch.token());
    assert_eq!(head.attempts, 1);
    tx.rollback().await.unwrap();
    assert!(
        r.pipeline
            .finish_native(
                &b,
                &dispatch,
                Ok(MessageProgress::Accepted(HostReceipt::default()))
            )
            .await
            .unwrap()
    );
    drop(sender);
    r.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn original_five_second_prepare_budget_cancels_queued_work_without_admission() {
    let h = Arc::new(Host::default());
    let r = Running::with_host(options(), None, h.clone()).await;
    let (mut sender, a) = start(&r, &h, "author").await;
    let (_receiver, b) = start(&r, &h, "recipient").await;
    drain(&r, &b).await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@recipient queued".into(),
        },
    )
    .await;
    let barrier = hold(&r, &h, &a).await;
    let started = std::time::Instant::now();
    assert_eq!(
        r.pipeline.prepare_native(&b).await.unwrap_err().code,
        agentchat_core::ErrorCode::RuntimeUnavailable
    );
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(6));
    h.release.notify_one();
    barrier.await.unwrap();
    let _ = health(&mut sender).await;
    let store = r.f.manager.open(&r.project).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let agent = tx
        .conversation(b.host, &b.host_session_id)
        .await
        .unwrap()
        .unwrap();
    let generation = tx
        .latest_launch(agent.id)
        .await
        .unwrap()
        .unwrap()
        .identity
        .generation;
    let stream = tx.message_stream(generation).await.unwrap().unwrap();
    assert!(stream.head.is_none());
    let item = tx
        .next_native(agent.id, stream.settled_seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.delivery().state, DeliveryState::Pending);
    assert_eq!(item.delivery().attempts, 0);
    tx.rollback().await.unwrap();
    drop(sender);
    r.stop().await;
}
