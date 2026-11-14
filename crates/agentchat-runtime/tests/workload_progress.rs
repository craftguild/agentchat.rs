//! No recipient can borrow another's progress or report an unresolved head done.
#[path = "workload/progress.rs"]
#[allow(dead_code)]
mod drain_progress;
mod support;
use drain_progress::{Claim, Guard, Participant, Progress, QueueProof, STALL};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

fn participant(now: Instant) -> Participant {
    Participant {
        native: 0,
        seq: 0,
        read: 0,
        positive_at: now,
        stage: "receiving",
        claim: None,
        generation: None,
    }
}
fn claim() -> Claim {
    Claim {
        delivery: Uuid::new_v4(),
        token: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        attempt: 1,
    }
}
fn advance(state: &mut Participant, at: Instant) {
    state.native += 1;
    state.seq += 1;
    state.positive_at = at;
}

#[test]
fn other_recipient_and_stage_activity_cannot_hide_stopped_receiver() {
    let start = Instant::now();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let mut states = BTreeMap::from([(a, participant(start)), (b, participant(start))]);
    let mut guard = Guard::new(start, false, 1, &states);
    for seconds in [10, 20, 29] {
        states.get_mut(&a).unwrap().stage = "flush_oneshot_wait";
        advance(
            states.get_mut(&b).unwrap(),
            start + Duration::from_secs(seconds),
        );
        assert!(
            guard
                .check(
                    start + Duration::from_secs(seconds),
                    &states,
                    &BTreeMap::new()
                )
                .is_ok()
        );
    }
    advance(states.get_mut(&b).unwrap(), start + STALL);
    assert!(
        guard
            .check(start + STALL, &states, &BTreeMap::new())
            .unwrap_err()
            .contains(&a.to_string())
    );
}

#[test]
fn successful_native_without_successful_read_is_not_complete() {
    let start = Instant::now();
    let a = Uuid::new_v4();
    let mut state = participant(start);
    state.native = 1;
    state.seq = 1;
    state.stage = "explicit_read_rpc";
    let states = BTreeMap::from([(a, state)]);
    let proofs = BTreeMap::from([(
        a,
        QueueProof {
            empty: true,
            generation: None,
            head: None,
        },
    )]);
    let mut guard = Guard::new(start, false, 1, &states);
    assert!(guard.check(start, &states, &proofs).is_ok());
    assert!(!guard.all_done());
    assert!(
        guard
            .check(start + STALL, &states, &proofs)
            .unwrap_err()
            .contains(&a.to_string())
    );
}

#[test]
fn completed_receiver_is_exempt_until_new_notice_rearms_its_own_guard() {
    let start = Instant::now();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let mut done = participant(start);
    done.read = 1;
    let mut states = BTreeMap::from([(a, done), (b, participant(start))]);
    let mut proofs = BTreeMap::from([(
        a,
        QueueProof {
            empty: true,
            generation: None,
            head: None,
        },
    )]);
    let mut guard = Guard::new(start, false, 1, &states);
    for seconds in [0, 20, 40, 60] {
        advance(
            states.get_mut(&b).unwrap(),
            start + Duration::from_secs(seconds),
        );
        assert!(
            guard
                .check(start + Duration::from_secs(seconds), &states, &proofs)
                .is_ok()
        );
    }
    proofs.get_mut(&a).unwrap().empty = false;
    assert!(
        guard
            .check(start + Duration::from_secs(61), &states, &proofs)
            .is_ok()
    );
    advance(states.get_mut(&b).unwrap(), start + Duration::from_secs(80));
    assert!(
        guard
            .check(start + Duration::from_secs(80), &states, &proofs)
            .is_ok()
    );
    advance(states.get_mut(&b).unwrap(), start + Duration::from_secs(91));
    assert!(
        guard
            .check(start + Duration::from_secs(91), &states, &proofs)
            .unwrap_err()
            .contains(&a.to_string())
    );
}

#[test]
fn unknown_original_head_cannot_hide_behind_empty_inbox_and_read_target() {
    let start = Instant::now();
    let a = Uuid::new_v4();
    let original = claim();
    let mut state = participant(start);
    state.read = 1;
    let states = BTreeMap::from([(a, state)]);
    let proofs = BTreeMap::from([(
        a,
        QueueProof {
            empty: true,
            generation: None,
            head: Some(original.clone()),
        },
    )]);
    let mut guard = Guard::new(start, false, 1, &states);
    assert!(guard.check(start, &states, &proofs).is_ok());
    assert!(!guard.all_done());
    let error = guard.check(start + STALL, &states, &proofs).unwrap_err();
    assert!(error.contains(&original.token.to_string()));
    assert_eq!(proofs[&a].head.as_ref(), Some(&original));
}

#[test]
fn mac_absolute_limit_is_retained_while_linux_requires_real_own_progress() {
    let start = Instant::now();
    let a = Uuid::new_v4();
    let mut states = BTreeMap::from([(a, participant(start))]);
    let mut mac = Guard::new(start, true, 1, &states);
    let mut linux = Guard::new(start, false, 1, &states);
    for seconds in [15, 29] {
        advance(
            states.get_mut(&a).unwrap(),
            start + Duration::from_secs(seconds),
        );
        assert!(
            mac.check(
                start + Duration::from_secs(seconds),
                &states,
                &BTreeMap::new()
            )
            .is_ok()
        );
        assert!(
            linux
                .check(
                    start + Duration::from_secs(seconds),
                    &states,
                    &BTreeMap::new()
                )
                .is_ok()
        );
    }
    advance(states.get_mut(&a).unwrap(), start + Duration::from_secs(31));
    assert!(
        mac.check(start + Duration::from_secs(31), &states, &BTreeMap::new())
            .unwrap_err()
            .contains("absolute30s")
    );
    assert!(
        linux
            .check(start + Duration::from_secs(31), &states, &BTreeMap::new())
            .is_ok()
    );
}

#[tokio::test]
async fn independent_guard_runs_while_flush_future_never_replies() {
    let fixture = support::Fixture::new();
    let project = fixture
        .manager
        .initialize(&fixture.workspace, fixture.request())
        .await
        .unwrap()
        .project;
    let storage = fixture.manager.open(&project).await.unwrap();
    let progress = Progress::new();
    let agent = Uuid::new_v4();
    let receiver = progress.register(agent, Uuid::new_v4());
    receiver.stage("flush_send_or_oneshot_pending");
    // Controlled aged guard only: no product clock/rate/deadline is changed.
    let guard = Guard::new(Instant::now() - STALL, false, 1, &progress.snapshot());
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        drain_progress::monitor_guarded(
            progress,
            storage.as_ref(),
            guard,
            BTreeMap::new(),
            std::future::pending(),
        ),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().contains(&agent.to_string()));
}

#[test]
fn empty_successor_generation_cannot_complete_the_original_receiver() {
    let start = Instant::now();
    let agent = Uuid::new_v4();
    let original = Uuid::new_v4();
    let mut state = participant(start);
    state.read = 1;
    state.generation = Some(original);
    let states = BTreeMap::from([(agent, state)]);
    let proofs = BTreeMap::from([(
        agent,
        QueueProof {
            empty: true,
            head: None,
            generation: Some(Uuid::new_v4()),
        },
    )]);
    let error = Guard::new(start, false, 1, &states)
        .check(start, &states, &proofs)
        .unwrap_err();
    assert!(error.contains("original generation changed"));
    assert!(error.contains(&original.to_string()));
}

#[tokio::test]
async fn completed_receivers_cannot_extend_pending_health_or_ro_control_forever() {
    let fixture = support::Fixture::new();
    let project = fixture
        .manager
        .initialize(&fixture.workspace, fixture.request())
        .await
        .unwrap()
        .project;
    let storage = fixture.manager.open(&project).await.unwrap();
    let progress = Progress::new();
    let agent = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let receiver = progress.register(agent, generation);
    receiver.read(1);
    progress.main_stage("health_or_readonly_control_pending", None);
    let states = progress.snapshot();
    let proofs = BTreeMap::from([(
        agent,
        QueueProof {
            empty: true,
            head: None,
            generation: Some(generation),
        },
    )]);
    // Controlled initial proof and aged guard exercise the real select loop,
    // without spending30s or querying/fabricating any business receipt.
    let start = Instant::now() - STALL;
    let mut guard = Guard::new(start, false, 1, &states);
    guard.check(start, &states, &proofs).unwrap();
    let deadline = guard.deadline();
    assert!(guard.all_done());
    progress.main_stage("different_stage_still_not_progress", None);
    assert_eq!(guard.deadline(), deadline);
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        drain_progress::monitor_guarded(
            progress,
            storage.as_ref(),
            guard,
            proofs,
            std::future::pending(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("control wait30s"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_unread_receiver_observation_cannot_block_another_real_commit_read_or_guard() {
    use agentchat_core::{application::Mutation, ports::Storage, read::Query, sequence::Cursor};
    use agentchat_protocol::rpc::{Call, Client, success};
    use agentchat_runtime::{context::ContextRegistry, daemon::Server};
    use std::sync::Arc;
    use support::runtime::{NativeObservation, accept_native_observed, mutate, options, query};
    let f = support::Fixture::new();
    let mut request = f.request();
    request.receipt_settings.auto_read = Some(false);
    let project = f
        .manager
        .initialize(&f.workspace, request)
        .await
        .unwrap()
        .project;
    let registry = ContextRegistry::new(f.manager.clone());
    let host = Arc::new(support::native_host::NativeHost::default());
    let mut contexts = Vec::new();
    for name in ["slot-sender", "held-a", "progressing-b"] {
        let c = registry
            .issue(support::runtime::seed(&f, name))
            .await
            .unwrap();
        host.bind(&c);
        contexts.push(c);
    }
    let server = Server::bind(
        f.manager.clone(),
        project.clone(),
        host,
        Arc::new(support::runtime::ManualClock::default()),
        options(),
    )
    .await
    .unwrap();
    let app = server.application();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let mut clients = Vec::new();
    let mut agents = Vec::new();
    for (i, c) in contexts.iter().enumerate() {
        let mut client = Client::connect(&handle.paths().socket, project.identity().id)
            .await
            .unwrap();
        let joined = success(
            client
                .call(
                    Uuid::new_v4(),
                    Call::StartLaunch {
                        context_id: c.context_id,
                        name: Some(format!("slot{i}")),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        let agent = serde_json::from_value(joined["agent_id"].clone()).unwrap();
        let generation = serde_json::from_value(joined["generation"].clone()).unwrap();
        agents.push((agent, generation));
        clients.push(client);
    }
    // The sender has no workload receiving obligation in this focused control.
    // A/B alone are monitored; each has a real unread body before the hold.
    let monitored = Progress::new();
    let a = monitored.register(agents[1].0, agents[1].1);
    let b = monitored.register(agents[2].0, agents[2].1);
    let posted = mutate(
        &mut clients[0],
        &contexts[0],
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@slot1 @slot2 original observer isolation".into(),
        },
    )
    .await;
    let message: Uuid = serde_json::from_value(posted["message_id"].clone()).unwrap();
    let store = f.manager.open(&project).await.unwrap();
    assert!(
        !store
            .snapshot()
            .await
            .unwrap()
            .inbox(agents[1].0, Cursor::START, 1)
            .await
            .unwrap()
            .is_empty()
    );
    let (entered, held) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _held = a.state.lock().unwrap();
        entered.send(()).unwrap();
        // Test-only watchdog. No product or workload deadline is changed.
        released.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    held.recv_timeout(Duration::from_secs(2)).unwrap();
    // This is the first snapshot ever: unavailable A must use its registered
    // original generation/zero counters without waiting or inventing progress.
    let reader = monitored.clone();
    let initial = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || reader.snapshot()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(initial[&agents[1].0].generation, Some(agents[1].1));
    assert_eq!(
        (initial[&agents[1].0].native, initial[&agents[1].0].read),
        (0, 0)
    );
    let start = Instant::now();
    let mut guard = Guard::new(start, false, 1, &initial);
    tokio::time::timeout(Duration::from_secs(2), async {
        let inbox = app
            .read(
                Some(&contexts[2]),
                agentchat_core::read::requests::Inbox {
                    page: Default::default(),
                },
            )
            .await
            .unwrap();
        let ids = inbox.items.iter().map(|item| item.delivery.id).collect();
        accept_native_observed(&app, &contexts[2], ids, |event| match event {
            NativeObservation::Preparing => b.stage("native_prepare"),
            NativeObservation::Prepared(d) => b.prepared(d),
            NativeObservation::Committed(d) => b.native(d),
        })
        .await;
        mutate(
            &mut clients[2],
            &contexts[2],
            Mutation::Read {
                message_ids: vec![message],
            },
        )
        .await;
        b.read(1);
    })
    .await
    .unwrap();
    let reader = monitored.clone();
    let states = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || reader.snapshot()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(states[&agents[2].0].native > 0);
    assert_eq!(states[&agents[2].0].read, 1);
    assert_eq!(
        states[&agents[1].0].positive_at,
        initial[&agents[1].0].positive_at
    );
    let mut snapshot = store.snapshot().await.unwrap();
    let mut proofs = BTreeMap::new();
    for (agent, generation) in &agents[1..] {
        let empty = snapshot
            .inbox(*agent, Cursor::START, 1)
            .await
            .unwrap()
            .is_empty();
        let stream = snapshot.message_stream(*generation).await.unwrap().unwrap();
        assert!(stream.head.is_none());
        proofs.insert(
            *agent,
            QueueProof {
                empty,
                head: None,
                generation: Some(*generation),
            },
        );
    }
    drop(snapshot);
    assert!(!proofs[&agents[1].0].empty);
    assert!(proofs[&agents[2].0].empty);
    assert!(guard.check(start, &states, &proofs).is_ok());
    let error = guard.check(start + STALL, &states, &proofs).unwrap_err();
    assert!(error.contains(&agents[1].0.to_string()));
    release.send(()).unwrap();
    holder.join().unwrap();
    let detail = query(
        &mut clients[0],
        None,
        Query::Message {
            message_id: message,
            receipts: Default::default(),
        },
    )
    .await;
    let rows = detail["receipts"]["items"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["recipient"]["actor_id"] == serde_json::json!(agents[2].0))
        .unwrap();
    assert!(
        row["reads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "explicit")
    );
    handle.shutdown();
    task.await.unwrap().unwrap();
}
