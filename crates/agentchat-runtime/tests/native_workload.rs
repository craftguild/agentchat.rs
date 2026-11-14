//! Managed full-input workload; synthetic owners never admit a real host.
#[path = "native_workload/actors.rs"]
mod actors;
#[path = "workload/audit.rs"]
mod audit;
#[path = "native_workload/capture.rs"]
mod capture;
#[path = "native_workload/host.rs"]
mod host;
#[path = "workload/http_viewer.rs"]
mod http_viewer;
mod support;
#[path = "workload/viewer.rs"]
mod viewer;
use agentchat_core::{
    model::Context,
    ports::Storage,
    read::{Checkpoint, requests},
    time::SystemClock,
};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::{
    context::ContextRegistry,
    daemon::{RuntimeHandle, RuntimeOptions, Server},
    project::Project,
};
use capture::{Capture, Input, SyntheticPort};
use serde_json::json;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use support::Fixture;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};
use uuid::Uuid;

struct Rig {
    f: Fixture,
    project: Project,
    contexts: Vec<Context>,
    handle: RuntimeHandle,
    task: Option<JoinHandle<agentchat_core::Result<()>>>,
}
impl Rig {
    async fn client(&self) -> Client {
        Client::connect(&self.handle.paths().socket, self.project.identity().id)
            .await
            .unwrap()
    }
    async fn stop(&mut self) {
        self.handle.shutdown();
        self.task.take().unwrap().await.unwrap().unwrap();
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.handle.shutdown();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    Smoke,
    Preflight,
    TenMinute,
}
impl Profile {
    fn parameters(self) -> (usize, usize) {
        match self {
            Self::Smoke => (5, 5),
            // Keep the full profile's ten-second side-operation interval.
            Self::Preflight => (60, 20),
            Self::TenMinute => (600, 20),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Preflight => "preflight",
            Self::TenMinute => "ten_minute",
        }
    }
}

async fn workload(profile: Profile) {
    let (seconds, side_every) = profile.parameters();
    let strict = profile != Profile::Smoke;
    let timing_enforced = strict && cfg!(target_os = "macos");
    let output = std::env::var_os("AGENTCHAT_WORKLOAD_OUTPUT").map(std::path::PathBuf::from);
    if strict {
        assert!(
            output.as_ref().is_some_and(|p| p.is_absolute()),
            "manual workload requires an explicit owned output directory"
        );
    }
    let mut f = output
        .as_ref()
        .map_or_else(Fixture::new, |p| Fixture::new_in(p));
    if output.is_some() {
        f.root.disable_cleanup(true);
    }
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let registry = ContextRegistry::new(f.manager.clone());
    let mut contexts = Vec::new();
    for index in 0..5 {
        contexts.push(
            registry
                .issue(support::runtime::seed(&f, &format!("workload-{index}")))
                .await
                .unwrap(),
        );
    }
    assert!(
        contexts
            .iter()
            .all(|c| c.working_directory == contexts[0].working_directory)
    );
    let host = Arc::new(host::ManagedHost::new(&contexts));
    let capture = Arc::new(Capture::new());
    let mut routes = HashMap::new();
    let mut receivers = Vec::new();
    for context in &contexts {
        let (send, receive) = mpsc::channel(4);
        routes.insert(context.context_id, send);
        receivers.push(receive);
    }
    let port = Arc::new(SyntheticPort {
        capture: capture.clone(),
        routes: routes.clone(),
        generations: host.generations(),
    });
    let server = Server::bind(
        f.manager.clone(),
        project.clone(),
        host,
        Arc::new(SystemClock),
        RuntimeOptions::default(),
    )
    .await
    .unwrap()
    .with_native_messages(port)
    .with_commit_observer(capture.clone());
    let mut rig = Rig {
        f,
        project,
        contexts,
        handle: server.handle(),
        task: Some(tokio::spawn(server.run())),
    };
    let (stop_anchors, anchor_stop) = watch::channel(false);
    let mut anchors = Vec::new();
    let mut consumers = Vec::new();
    let mut agent_ids = Vec::new();
    let progress = Arc::new(AtomicI64::new(0));
    let (stop_observer, observer_stop) = watch::channel(false);
    let follower = rig
        .client()
        .await
        .subscribe_changes(Checkpoint::start(rig.project.identity().id))
        .await
        .unwrap();
    let observer = tokio::spawn(audit::observe(
        follower,
        rig.client().await,
        progress.clone(),
        observer_stop,
    ));
    let viewer_lease = rig.client().await.observe().await.unwrap();
    let viewer_follower = rig
        .client()
        .await
        .subscribe_changes(Checkpoint::start(rig.project.identity().id))
        .await
        .unwrap();
    let viewer_progress = Arc::new(AtomicI64::new(0));
    let (stop_viewer, viewer_stop) = watch::channel(false);
    let viewer_task = tokio::spawn(viewer::observe(
        viewer_follower,
        viewer_progress.clone(),
        viewer_stop,
    ));
    let http_viewer = http_viewer::start(&rig.f, &rig.project, strict, output.as_deref()).await;
    let expected_observers = if http_viewer.is_some() { 5 } else { 3 };
    for (index, receive) in receivers.into_iter().enumerate() {
        let context = rig.contexts[index].clone();
        let mut client = rig.client().await;
        let joined = success(
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
        let agent = serde_json::from_value(joined["agent_id"].clone()).unwrap();
        agent_ids.push(agent);
        success(
            client
                .call(
                    Uuid::new_v4(),
                    Call::Attach {
                        context_id: context.context_id,
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        anchors.push(tokio::spawn(actors::anchor(
            client,
            context.clone(),
            anchor_stop.clone(),
        )));
        consumers.push(tokio::spawn(actors::consumer(
            rig.client().await,
            context,
            agent,
            receive,
        )));
    }
    agent_ids.sort();
    agent_ids.dedup();
    assert_eq!(agent_ids.len(), 5);
    let mut writers = Vec::new();
    let mut clients = Vec::new();
    let mut side_clients = Vec::new();
    for _ in 0..5 {
        clients.push(rig.client().await);
        side_clients.push(rig.client().await);
    }
    let start = Instant::now() + Duration::from_millis(250);
    capture.start(start.into_std());
    for (index, client) in clients.into_iter().enumerate() {
        writers.push(tokio::spawn(actors::writer(
            client,
            rig.contexts[index].clone(),
            index,
            seconds * 2,
            start,
            capture.clone(),
        )));
    }
    for (index, client) in side_clients.into_iter().enumerate() {
        writers.push(tokio::spawn(actors::side(
            client,
            rig.contexts[index].clone(),
            index,
            seconds * 2 / side_every,
            Duration::from_millis(side_every as u64 * 500),
            start,
        )));
    }
    let mut status_client = rig.client().await;
    let mut samples = Vec::new();
    let mut peak_jobs = 0;
    let mut peak_completions = 0;
    let end = start + Duration::from_secs(seconds as u64);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {_=tokio::time::sleep_until(end)=>break,_=tick.tick()=>{
            let status=actors::health(&mut status_client).await;
            assert_eq!(status["runtime"]["attached_connections"],5);assert_eq!(status["runtime"]["observers"],expected_observers);
            assert!(status["runtime"]["last_control_error"].is_null(),"{status}");
            let native=&status["runtime"]["native_delivery"];
            let jobs=native["jobs"].as_u64().unwrap();let completions=native["queued_completions"].as_u64().unwrap();
            assert!(jobs<=32&&completions<=32);peak_jobs=peak_jobs.max(jobs);peak_completions=peak_completions.max(completions);
            assert_eq!(native["max_completion_group"],8);
            assert!(native["largest_completion_group"].as_u64().unwrap()<=8);
            assert_eq!(status["runtime"]["diagnostics"]["state"],"running");
            assert_eq!(status["runtime"]["diagnostics"]["dropped_records"],0);
            check_logs(rig.project.data_dir());samples.push(status["statistics"]["messages"].as_u64().unwrap());
            if samples.len()%30==0 {println!("WORKLOAD_PROGRESS {} {}",samples.len(),samples.last().unwrap());}
        }}
    }
    for writer in writers {
        writer.await.unwrap();
    }
    let posting_elapsed = start.elapsed().as_secs_f64();
    let expected = seconds * 10;
    let channels = 1 + 5 * (seconds * 2 / side_every);
    let watch_store = rig.f.manager.open(&rig.project).await.unwrap();
    let drain = async {
        let mut progress = (0, 0);
        let mut last_advance = Instant::now();
        let mut last_report = Instant::now();
        loop {
            let mut replies = Vec::new();
            for context in &rig.contexts {
                let (send, receive) = oneshot::channel();
                routes[&context.context_id]
                    .send(Input::Flush(send))
                    .await
                    .unwrap();
                replies.push(receive);
            }
            let mut fully_read = 0;
            for reply in replies {
                fully_read += reply.await.unwrap();
            }
            assert!(fully_read <= expected * 4);
            let status = actors::health(&mut status_client).await;
            let settled = status["statistics"]["deliveries"].as_u64().unwrap()
                - status["statistics"]["pending_deliveries"].as_u64().unwrap();
            assert!(fully_read >= progress.0 && settled >= progress.1);
            if (fully_read, settled) != progress {
                progress = (fully_read, settled);
                last_advance = Instant::now();
            }
            // The finite read set and immutable event fan-out bound both
            // monotonic counters. Detect stopped progress without imposing a
            // Linux throughput target on a still-draining, bounded FIFO.
            assert!(
                last_advance.elapsed() < Duration::from_secs(30),
                "native workload stopped draining: {status}"
            );
            if last_report.elapsed() >= Duration::from_secs(30) {
                println!(
                    "WORKLOAD_DRAIN_PROGRESS read={fully_read} settled={settled} pending={}",
                    status["statistics"]["pending_deliveries"]
                );
                last_report = Instant::now();
            }
            // Native settlement and automatic read are independent from the model's
            // explicit declaration. Receipt writes create no passive event.
            if fully_read == expected * 4 && status["statistics"]["pending_deliveries"] == 0 {
                let mut tx = watch_store.begin().await.unwrap();
                let due = tx
                    .due_deliveries(agentchat_core::time::Clock::now(&SystemClock))
                    .await
                    .unwrap();
                tx.rollback().await.unwrap();
                // Existing maintenance must also settle a bounded due backlog;
                // its author-only notices then drain through the ordinary FIFO.
                if due.is_empty() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    if cfg!(target_os = "macos") {
        tokio::time::timeout(Duration::from_secs(30), drain)
            .await
            .unwrap();
    } else {
        drain.await;
    }
    Arc::try_unwrap(watch_store)
        .ok()
        .unwrap()
        .close()
        .await
        .unwrap();
    let status = actors::health(&mut status_client).await;
    let latency = capture.report(expected);
    if let Some(output) = &output {
        // Retain timing evidence even if a later accounting/reopen assertion
        // fails. This provisional artifact never means the gate passed.
        std::fs::write(
            output.join("dispatch.json"),
            serde_json::to_vec_pretty(&latency).unwrap(),
        )
        .unwrap();
    }
    let head = status["head"].as_str().unwrap().parse::<i64>().unwrap();
    let viewer_head = success(
        status_client
            .read(Uuid::new_v4(), None, requests::ViewerHead)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(viewer_head.seq.get(), head);
    tokio::time::timeout(Duration::from_secs(10), async {
        while progress.load(Ordering::SeqCst) < head
            || viewer_progress.load(Ordering::SeqCst) < head
            || http_viewer
                .as_ref()
                .is_some_and(|v| v.progress.load(Ordering::SeqCst) < head)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Close the real viewer with its SSE source active. Its public shutdown path
    // must leave the shared workload runtime and all native attachments alive.
    let http_observed = match http_viewer {
        Some(viewer) => Some(viewer.close().await),
        None => None,
    };
    assert!(!rig.task.as_ref().unwrap().is_finished());
    stop_observer.send_replace(true);
    let observed = observer.await.unwrap();
    assert_eq!(observed.head, head);
    stop_viewer.send_replace(true);
    let viewer_observed = viewer_task.await.unwrap();
    drop(viewer_lease);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = actors::health(&mut status_client).await;
            if status["runtime"]["observers"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for context in &rig.contexts {
        routes[&context.context_id].send(Input::Stop).await.unwrap();
    }
    let mut reads = 0;
    for consumer in consumers {
        reads += consumer.await.unwrap();
    }
    assert_eq!(reads, expected * 4);
    stop_anchors.send_replace(true);
    for anchor in anchors {
        anchor.await.unwrap();
    }
    let store = rig.f.manager.open(&rig.project).await.unwrap();
    let before = store.snapshot().await.unwrap().statistics().await.unwrap();
    Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
    rig.stop().await;
    let store = rig.f.manager.open(&rig.project).await.unwrap();
    let after = store.snapshot().await.unwrap().statistics().await.unwrap();
    assert_eq!(before, after);
    let accounting = audit::retained(&store, &observed, expected, channels, true).await;
    let viewer_accounting = viewer::retained(&store, viewer_observed, viewer_head).await;
    let http_accounting = http_viewer::retained(&store, http_observed).await;
    Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
    check_logs(rig.project.data_dir());
    if profile == Profile::TenMinute {
        assert!(
            rig.project.data_dir().join("logs/runtime.jsonl.2").exists(),
            "full workload must exercise actual rotation"
        );
    }
    let rpc_timings = actors::timings();
    assert!(rpc_timings.get("ack").is_none());
    assert!(rpc_timings.get("inbox").is_none());
    let report = json!({"version":1,"profile":profile.name(),"seconds":seconds,"messages":expected,
        "mutation_groups":status["runtime"]["mutation_groups"],
        "native_delivery":status["runtime"]["native_delivery"],
        "route":"managed_full_input_and_notices",
        "model_delivery_acks":0,"model_body_fetches":0,
        "native_completion_groups":{"groups":status["runtime"]["native_delivery"]["completion_groups"],
            "largest":status["runtime"]["native_delivery"]["largest_completion_group"],
            "limit":status["runtime"]["native_delivery"]["max_completion_group"]},
        "posting_phase_seconds":posting_elapsed,"participants":5,"target_messages_per_second":10,
        "local_dispatch":latency,"accounting":accounting,"viewer_observer":viewer_accounting,
        "viewer_http":http_accounting,
        "message_count_samples":samples,"rpc_timings":rpc_timings,
        "timing_enforced":timing_enforced,
        "latency_target_passed":latency["p95_ms"].as_f64().unwrap()<250.0,
        "posting_window_passed":latency["last_commit_offset_ms"].as_f64().unwrap()<=seconds as f64*1000.0,
        "peak_native_jobs":peak_jobs,"peak_native_completions":peak_completions,
        "fixture_root":rig.f.root.path(),"agentchat_home":rig.f.manager.home(),"configuration_root":rig.f.config,
        "database":rig.project.database_path(),"logs":rig.project.data_dir().join("logs")});
    if let Some(output) = output {
        std::fs::write(
            output.join("workload.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    println!(
        "WORKLOAD_COMPLETE {}",
        serde_json::to_string(&report).unwrap()
    );
    // Evaluate timing last, on macOS only. Linux retains both numeric results
    // and the same durable accounting, observer and stop/reopen assertions.
    if timing_enforced {
        assert!(
            report["posting_window_passed"].as_bool().unwrap(),
            "message producer fell behind the fixed load window"
        );
        assert!(
            report["latency_target_passed"].as_bool().unwrap(),
            "commit return to local dispatch p95 {} ms",
            report["local_dispatch"]["p95_ms"]
        );
    }
}
fn check_logs(data: &Path) {
    let mut count = 0;
    for entry in std::fs::read_dir(data.join("logs")).unwrap() {
        let entry = entry.unwrap();
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("runtime.jsonl")
        {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            assert!(meta.len() <= agentchat_runtime::diagnostics::FILE_BYTES as u64);
            count += 1;
        }
    }
    assert!(count <= agentchat_runtime::diagnostics::FILE_COUNT);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_participant_native_workload_smoke() {
    workload(Profile::Smoke).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual preflight: five participants, full operation rates, one minute"]
async fn one_minute_native_workload_preflight() {
    workload(Profile::Preflight).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual acceptance gate: five participants, 10 messages/s, ten minutes"]
async fn ten_minute_native_workload() {
    workload(Profile::TenMinute).await;
}
