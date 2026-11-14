//! Atomic lease conflicts while five managed synthetic participants exchange full inputs.
#[path = "native_workload/host.rs"]
mod host;
mod support;
use agentchat_core::{
    ErrorCode, Result,
    application::Mutation,
    delivery::{
        HostReceipt, MessageDispatch, MessageInput, MessagePort, MessageProgress, NoticeDispatch,
    },
    glob::Glob,
    model::Context,
    read::{PageRequest, Query},
};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::{
    context::ContextRegistry,
    daemon::{RuntimeOptions, Server},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{
    Fixture,
    runtime::{ManualClock, attach, mutate, query, seed},
};
use tokio::{sync::Barrier, time::Instant};
use uuid::Uuid;

#[derive(Default)]
struct Inputs {
    bodies: Mutex<Vec<(Uuid, MessageInput)>>,
    notices: AtomicUsize,
    generations: HashMap<Uuid, Uuid>,
}
#[async_trait::async_trait]
impl MessagePort for Inputs {
    async fn present_message(
        &self,
        context: &Context,
        dispatch: &MessageDispatch,
    ) -> Result<MessageProgress> {
        assert_eq!(
            dispatch.input.generation,
            self.generations[&context.context_id]
        );
        assert!(dispatch.input.auto_read);
        assert_eq!(dispatch.attempt, 1);
        assert!(!dispatch.awaiting_acceptance);
        let text = dispatch.input.text()?;
        assert_eq!(MessageInput::from_text(&text)?, dispatch.input);
        self.bodies
            .lock()
            .unwrap()
            .push((context.context_id, dispatch.input.clone()));
        Ok(MessageProgress::Accepted(HostReceipt::default()))
    }
    async fn present_notice(
        &self,
        context: &Context,
        dispatch: &NoticeDispatch,
    ) -> Result<MessageProgress> {
        assert_eq!(
            dispatch.input.generation,
            self.generations[&context.context_id]
        );
        assert!(dispatch.input.auto_read);
        assert_eq!(dispatch.attempt, 1);
        assert!(!dispatch.awaiting_acceptance);
        self.notices.fetch_add(1, Ordering::SeqCst);
        Ok(MessageProgress::Accepted(HostReceipt::default()))
    }
}
fn reserve(patterns: &[&str]) -> Mutation {
    Mutation::Reserve {
        patterns: patterns.iter().map(|value| (*value).into()).collect(),
        ttl_seconds: Some(600),
        reason: Some("five-participant conflict proof".into()),
    }
}
async fn conflict(client: &mut Client, context: &Context, patterns: &[&str]) {
    let key = Uuid::new_v4();
    let error = client
        .call(
            key,
            Call::Mutate {
                request_id: key,
                actor: agentchat_protocol::rpc::ActorReference::Native {
                    context_id: context.context_id,
                },
                command: reserve(patterns),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ReservationConflict);
    assert!(!error.retryable);
    assert!(!error.details["conflicts"].as_array().unwrap().is_empty());
    for row in error.details["conflicts"].as_array().unwrap() {
        let path = row["witness_path"].as_str().unwrap();
        for name in ["existing_pattern", "requested_pattern"] {
            assert!(
                Glob::compile(row[name].as_str().unwrap())
                    .unwrap()
                    .is_match(path)
            );
        }
    }
}
async fn reservations(client: &mut Client) -> Value {
    query(
        client,
        None,
        Query::Reservations {
            owner: None,
            all: true,
            page: PageRequest {
                limit: Some(100),
                after: Default::default(),
            },
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overlapping_groups_rollback_atomically_during_five_participant_full_input_load() {
    let fixture = Fixture::new();
    let project = fixture
        .manager
        .initialize(&fixture.workspace, fixture.request())
        .await
        .unwrap()
        .project;
    let registry = ContextRegistry::new(fixture.manager.clone());
    let mut contexts = Vec::new();
    for index in 0..5 {
        contexts.push(
            registry
                .issue(seed(&fixture, &format!("reservation-load-{index}")))
                .await
                .unwrap(),
        );
    }
    let host = Arc::new(host::ManagedHost::new(&contexts));
    let inputs = Arc::new(Inputs {
        generations: host.generations(),
        ..Default::default()
    });
    // Frozen domain time keeps this focused on conflict isolation, not expiry;
    // real native renewal/retirement is verified in the separate host matrix.
    let server = Server::bind(
        fixture.manager.clone(),
        project.clone(),
        host,
        Arc::new(ManualClock::default()),
        RuntimeOptions::default(),
    )
    .await
    .unwrap()
    .with_native_messages(inputs.clone());
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let socket = handle.paths().socket.clone();
    let project_id = project.identity().id;
    let mut clients = Vec::new();
    for (index, context) in contexts.iter().enumerate() {
        let mut client = Client::connect(&socket, project_id).await.unwrap();
        success(
            client
                .call(
                    Uuid::new_v4(),
                    Call::StartLaunch {
                        context_id: context.context_id,
                        name: Some(format!("load{index}")),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        attach(&mut client, context).await;
        clients.push(client);
    }
    let mut control = Client::connect(&socket, project_id).await.unwrap();
    // One live RPC stream is bound to one exact context. Control operations
    // need the same separation as each participant's independent writer.
    let mut owners = Vec::new();
    for _ in &contexts {
        owners.push(Client::connect(&socket, project_id).await.unwrap());
    }
    let held = mutate(&mut owners[0], &contexts[0], reserve(&["protected/**"])).await;
    let stop = Arc::new(AtomicBool::new(false));
    let posted = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(Barrier::new(6));
    let mut writers = Vec::new();
    for (index, mut client) in clients.into_iter().enumerate() {
        let context = contexts[index].clone();
        let stop = stop.clone();
        let posted = posted.clone();
        let ready = ready.clone();
        writers.push(tokio::spawn(async move {
            let mut messages = Vec::new();
            loop {
                let body = format!(
                    "@load{} Reservation load {} {} 日本語\0\n",
                    (index + 1) % 5,
                    index,
                    messages.len()
                );
                let result = mutate(
                    &mut client,
                    &context,
                    Mutation::Post {
                        channel: Some("dev".into()),
                        body: body.clone(),
                    },
                )
                .await;
                assert!(result["warnings"].as_array().unwrap().is_empty());
                let id: Uuid = serde_json::from_value(result["message_id"].clone()).unwrap();
                messages.push((id, body));
                posted.fetch_add(1, Ordering::SeqCst);
                if messages.len() <= 2 {
                    ready.wait().await;
                }
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                assert!(
                    messages.len() < 200,
                    "conflict controller failed to stop bounded writers"
                );
            }
            (messages, client)
        }));
    }
    ready.wait().await;
    ready.wait().await;
    let before = posted.load(Ordering::SeqCst);
    conflict(
        &mut owners[1],
        &contexts[1],
        &["free-after-failure/**", "protected/future.rs"],
    )
    .await;
    // A THIRD owner acquires the free half: same-owner overlap would otherwise
    // conceal a partially committed failed group.
    let independent = mutate(
        &mut owners[2],
        &contexts[2],
        reserve(&["free-after-failure/**"]),
    )
    .await;
    conflict(
        &mut owners[3],
        &contexts[3],
        &["second-free/**", "free-after-failure/new.rs"],
    )
    .await;
    let second = mutate(&mut owners[4], &contexts[4], reserve(&["second-free/**"])).await;
    let snapshot = reservations(&mut control).await;
    let rows = snapshot["items"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let ids: Vec<_> = rows
        .iter()
        .map(|row| row["reservation_id"].clone())
        .collect();
    for group in [&held, &independent, &second] {
        assert!(ids.contains(&group["reservation_id"]));
    }
    assert!(
        rows.iter()
            .all(|row| row["patterns"].as_array().unwrap().len() == 1)
    );
    assert!(
        posted.load(Ordering::SeqCst) > before,
        "messages did not progress during reservation conflicts"
    );
    for (index, group) in [(0, &held), (2, &independent), (4, &second)] {
        mutate(
            &mut owners[index],
            &contexts[index],
            Mutation::ReservationsRelease {
                reservation_id: serde_json::from_value(group["reservation_id"].clone()).unwrap(),
                generation: group["generation"].as_i64().unwrap(),
            },
        )
        .await;
    }
    stop.store(true, Ordering::SeqCst);
    let mut expected = HashMap::new();
    let mut anchors = Vec::new();
    for writer in writers {
        let (messages, client) = writer.await.unwrap();
        anchors.push(client);
        assert!(messages.len() >= 2);
        expected.extend(messages);
    }
    let mut progress = 0;
    let mut changed = Instant::now();
    loop {
        let status = support::runtime::health(&mut control).await;
        if status["statistics"]["pending_deliveries"] == 0
            && status["runtime"]["native_delivery"]["jobs"] == 0
            && status["runtime"]["native_delivery"]["queued_completions"] == 0
        {
            assert_eq!(
                status["statistics"]["read_receipts"].as_u64().unwrap(),
                (expected.len() * 4) as u64
            );
            assert_eq!(
                status["statistics"]["messages"].as_u64().unwrap(),
                expected.len() as u64
            );
            assert!(
                status["statistics"]
                    .get("acknowledged_deliveries")
                    .is_none()
            );
            break;
        }
        let current = inputs.bodies.lock().unwrap().len() + inputs.notices.load(Ordering::SeqCst);
        if current != progress {
            progress = current;
            changed = Instant::now();
        }
        assert!(
            changed.elapsed() < Duration::from_secs(30),
            "native load stopped making functional progress"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    {
        let bodies = inputs.bodies.lock().unwrap();
        assert_eq!(bodies.len(), expected.len() * 4);
        let mut recipients: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for (context, input) in bodies.iter() {
            // Posting normalizes boundary White_Space; interior bytes stay exact.
            assert_eq!(input.body, expected[&input.message_id].trim());
            recipients
                .entry(input.message_id)
                .or_default()
                .push(*context);
        }
        for targets in recipients.values_mut() {
            targets.sort();
            targets.dedup();
            assert_eq!(targets.len(), 4);
        }
    }
    let sample = *expected.keys().next().unwrap();
    support::runtime::assert_automatic_receipts(&mut control, sample, 4).await;
    assert!(
        reservations(&mut control).await["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "released")
    );
    drop(anchors);
    drop(owners);
    drop(control);
    handle.shutdown();
    task.await.unwrap().unwrap();
}
