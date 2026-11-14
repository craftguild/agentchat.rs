//! Test-only monotonic observations. These never grant production host admission.
use agentchat_core::{
    Error, ErrorCode, Result,
    delivery::{DeliveryPort, HostReceipt, MAX_WAKE_BYTES, MAX_WAKE_EVENTS, WakeDispatch},
    model::Context,
    sequence::Sequence,
};
use agentchat_runtime::daemon::CommitObserver;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub enum Input {
    Native(WakeDispatch),
    Flush(oneshot::Sender<usize>),
    Stop,
}
#[derive(Default)]
struct Data {
    epoch: Option<Instant>,
    requests: HashSet<Uuid>,
    commits: HashMap<i64, Instant>,
    dispatched: HashSet<i64>,
    timeout_targets: HashMap<Uuid, HashSet<Uuid>>,
    latency_ms: Vec<f64>,
    commit_offsets_ms: Vec<f64>,
    presentations: usize,
    max_refs: usize,
    max_bytes: usize,
    max_model_queue: usize,
}
pub struct Capture {
    data: Mutex<Data>,
}
impl Capture {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(Data::default()),
        }
    }
    pub fn start(&self, epoch: Instant) {
        assert!(self.data.lock().unwrap().epoch.replace(epoch).is_none());
    }
    pub fn track(&self, request: Uuid) {
        assert!(self.data.lock().unwrap().requests.insert(request));
    }
    pub fn timeout_count(&self) -> usize {
        self.data.lock().unwrap().timeout_targets.len()
    }
    pub fn assert_timeout_targets(&self, authors: &HashMap<Uuid, Uuid>) {
        let data = self.data.lock().unwrap();
        assert_eq!(data.timeout_targets.len(), authors.len());
        for (seq, author) in authors {
            assert_eq!(
                data.timeout_targets.get(seq),
                Some(&HashSet::from([*author]))
            );
        }
    }
    pub fn report(&self, expected: usize) -> Value {
        let d = self.data.lock().unwrap();
        assert!(d.requests.is_empty());
        assert!(d.commits.is_empty());
        assert_eq!(d.dispatched.len(), expected);
        assert_eq!(d.latency_ms.len(), expected);
        let mut samples = d.latency_ms.clone();
        samples.sort_by(f64::total_cmp);
        let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
        let last = d.commit_offsets_ms.iter().copied().fold(0.0, f64::max);
        let mut bins = HashMap::<usize, usize>::new();
        for sample in &d.commit_offsets_ms {
            *bins.entry((*sample / 1000.0) as usize).or_default() += 1;
        }
        json!({"samples":samples.len(),"p50_ms":samples[samples.len()/2],"p95_ms":p95,
            "timeout_hint_events":d.timeout_targets.len(),
            "max_ms":samples.last(),"last_commit_offset_ms":last,
            "commits_per_second":bins,"presentations":d.presentations,
            "max_hint_refs":d.max_refs,"max_hint_bytes":d.max_bytes,"max_model_queue":d.max_model_queue,
            "boundary":"application commit return to synthetic DeliveryPort entry",
            "real_host_acceptance":"not_measured","model_inference":"not_measured"})
    }
}
impl CommitObserver for Capture {
    fn committed(&self, request: Uuid, head: Sequence) {
        let now = Instant::now();
        let mut d = self.data.lock().unwrap();
        if d.requests.remove(&request) {
            // This fixture tracks only Post: its single transaction allocates
            // the Message Header immediately before its audit occurrence.
            assert!(head.get() > 1);
            assert!(d.commits.insert(head.get() - 1, now).is_none());
            let epoch = d.epoch.expect("workload epoch set before posting");
            d.commit_offsets_ms
                .push(now.saturating_duration_since(epoch).as_secs_f64() * 1000.0);
        }
    }
}
pub struct SyntheticPort {
    pub capture: Arc<Capture>,
    pub routes: HashMap<Uuid, mpsc::Sender<Input>>,
}
#[async_trait]
impl DeliveryPort for SyntheticPort {
    async fn present(&self, context: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        let entry = Instant::now();
        assert_eq!(context.host_version, "fixture-only");
        assert_eq!(context.os_profile, "synthetic-not-admitted");
        assert_eq!(dispatch.hint.context_id, context.context_id);
        let bytes = serde_json::to_vec(&dispatch.hint).unwrap().len();
        assert!(bytes <= MAX_WAKE_BYTES);
        assert!(dispatch.hint.events.len() <= MAX_WAKE_EVENTS);
        let route = self.routes.get(&context.context_id).unwrap();
        {
            let mut d = self.capture.data.lock().unwrap();
            d.presentations += 1;
            d.max_refs = d.max_refs.max(dispatch.hint.events.len());
            d.max_bytes = d.max_bytes.max(bytes);
            for reference in &dispatch.hint.events {
                if reference.kind == "message.delivery_confirmation_timeout" {
                    // Hint success and manual ACK never establish complete-input
                    // acceptance. Their author-only deadline hints are a second
                    // population, independent of original-post dispatch latency.
                    d.timeout_targets
                        .entry(reference.message_id)
                        .or_default()
                        .insert(dispatch.hint.agent_id);
                    continue;
                }
                assert_eq!(reference.kind, "message.created");
                if d.dispatched.insert(reference.seq.get()) {
                    let commit = d
                        .commits
                        .remove(&reference.seq.get())
                        .expect("committed before native scheduling");
                    d.latency_ms
                        .push(entry.duration_since(commit).as_secs_f64() * 1000.0);
                }
            }
        }
        // Full is bounded backpressure, not an extra admitted slot. Waiting
        // holds no writer or capture lock; the normal Runtime submission
        // deadline may cancel this future without publishing the hint.
        let permit = route
            .reserve()
            .await
            .map_err(|_| Error::new(ErrorCode::RuntimeUnavailable, "Synthetic receiver closed."))?;
        {
            let mut d = self.capture.data.lock().unwrap();
            d.max_model_queue = d
                .max_model_queue
                .max(route.max_capacity() - route.capacity());
        }
        // No suspension after reserving: enqueue the exact claim before
        // reporting transport acceptance. Full-input facts remain separate.
        permit.send(Input::Native(dispatch.clone()));
        Ok(HostReceipt::default())
    }
}
