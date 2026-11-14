//! Socket attachment is bounded by exact host evidence and process ownership.

mod support;
use agentchat_core::{application::Mutation, read::Query, time::Clock};
use agentchat_protocol::rpc::Call;
use std::time::Duration;
use support::runtime::{Running, attach, join, mutate, options, query};
use uuid::Uuid;

// Real FULL commits and host scheduling are not millisecond test clocks. Leave
// startup/descriptor I/O room to finish, then observe the actual terminal task
// with a separate bounded deadline instead of assuming cleanup takes one tick.
const IDLE: Duration = Duration::from_secs(2);
async fn idle_exit(r: &Running) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !r.finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("runtime must finish after its idle condition becomes true");
}

#[tokio::test]
async fn an_unjoined_attachment_or_unbound_socket_does_not_prevent_idle_exit() {
    let mut opts = options();
    opts.idle = IDLE;
    let r = Running::new(opts).await;
    let context = r.context("uninvited").await;
    let mut client = r.client().await;
    assert!(attach(&mut client, &context).await["agent"].is_null());
    idle_exit(&r).await;
    r.stop().await;
}

#[tokio::test]
async fn attached_connections_are_reference_counted_and_cannot_fake_fresh_host_liveness() {
    let mut opts = options();
    opts.idle = IDLE;
    let r = Running::new(opts).await;
    let context = r.context("retained").await;
    let mut first = r.client().await;
    join(&mut first, &context, "alice").await;
    attach(&mut first, &context).await;
    let mut second = r.client().await;
    attach(&mut second, &context).await;
    drop(first);
    tokio::time::sleep(IDLE + Duration::from_millis(200)).await;
    assert!(!r.finished());
    assert_eq!(
        query(&mut second, None, Query::Whoami).await["actor"]["connected"],
        true
    );
    // Merely retaining the helper/socket does not refresh actual host evidence.
    r.clock.advance(45);
    idle_exit(&r).await;
    drop(second);
    r.stop().await;
}

#[tokio::test]
async fn an_observer_keeps_the_runtime_alive_until_it_disconnects() {
    let mut opts = options();
    opts.idle = IDLE;
    let r = Running::new(opts).await;
    let follower = r.client().await.observe().await.unwrap();
    assert_eq!(follower.head().project_id, r.project.identity().id);
    tokio::time::sleep(IDLE + Duration::from_millis(200)).await;
    assert!(!r.finished());
    drop(follower);
    idle_exit(&r).await;
    r.stop().await;
}

#[tokio::test]
async fn normal_restart_disconnects_and_restore_requires_a_fresh_incarnation() {
    let mut r = Running::new(options()).await;
    let context = r.context("lease-owner").await;
    let mut client = r.client().await;
    join(&mut client, &context, "alice").await;
    mutate(
        &mut client,
        &context,
        Mutation::Reserve {
            patterns: vec!["src/**".into()],
            ttl_seconds: Some(30),
            reason: None,
        },
    )
    .await;
    r.restart(options()).await;
    drop(client);
    r.clock.advance(10);
    let mut inspect = r.client().await;
    let reservations = query(
        &mut inspect,
        None,
        Query::Reservations {
            owner: None,
            all: true,
            page: Default::default(),
        },
    )
    .await;
    let deadline = reservations["items"][0]["expires_at"].as_str().unwrap();
    assert_eq!(
        deadline,
        r.clock.now().after_seconds(20).unwrap().to_string()
    );
    assert_eq!(
        query(&mut inspect, Some(context.context_id), Query::Whoami).await["actor"]["connected"],
        false
    );
    let error = inspect
        .call(
            Uuid::new_v4(),
            Call::Attach {
                context_id: context.context_id,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, agentchat_core::ErrorCode::IdentityConflict);
    let fresh = r.context("lease-owner").await;
    assert_eq!(
        attach(&mut inspect, &fresh).await["agent"]["handle"],
        "alice"
    );
    assert_eq!(
        query(&mut inspect, None, Query::Whoami).await["actor"]["connected"],
        true
    );
    drop(inspect);
    r.stop().await;
}
