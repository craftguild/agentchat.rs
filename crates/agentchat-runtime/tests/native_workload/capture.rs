//! Test-only monotonic observations. These never grant production host admission.
use agentchat_core::{
    Result,
    delivery::{
        HostReceipt, MessageDispatch, MessageInput, MessagePort, MessageProgress, NoticeDispatch,
        NoticeInput,
    },
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

#[cfg(test)]
#[path = "attempts.rs"]
mod attempts;

pub enum Input {
    Body(Box<MessageInput>),
    Flush(oneshot::Sender<usize>),
    Stop,
}
#[derive(Default)]
struct Data {
    epoch: Option<Instant>,
    requests: HashSet<Uuid>,
    commits: HashMap<i64, Instant>,
    dispatched: HashSet<i64>,
    latency_ms: Vec<f64>,
    commit_offsets_ms: Vec<f64>,
    presentations: usize,
    max_bytes: usize,
    body_deliveries: HashSet<Uuid>,
    notice_deliveries: HashSet<Uuid>,
    last_sequences: HashMap<Uuid, i64>,
    max_model_queue: usize,
    max_claim_attempt: u32,
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
    pub fn report(&self, expected: usize) -> Value {
        let d = self.data.lock().unwrap();
        assert!(d.requests.is_empty());
        assert!(d.commits.is_empty());
        assert_eq!(d.dispatched.len(), expected);
        assert_eq!(d.latency_ms.len(), expected);
        assert_eq!(d.body_deliveries.len(), expected * 4);
        let mut samples = d.latency_ms.clone();
        samples.sort_by(f64::total_cmp);
        let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
        let last = d.commit_offsets_ms.iter().copied().fold(0.0, f64::max);
        let mut bins = HashMap::<usize, usize>::new();
        for sample in &d.commit_offsets_ms {
            *bins.entry((*sample / 1000.0) as usize).or_default() += 1;
        }
        json!({"samples":samples.len(),"p50_ms":samples[samples.len()/2],"p95_ms":p95,
            "max_ms":samples.last(),"last_commit_offset_ms":last,
            "commits_per_second":bins,"presentations":d.presentations,
            "complete_native_bodies":d.body_deliveries.len(),"native_notices":d.notice_deliveries.len(),"max_native_frame_bytes":d.max_bytes,"max_model_queue":d.max_model_queue,
            "max_reserved_attempt_at_native_entry":d.max_claim_attempt,
            "boundary":"application commit return to addressed synthetic full-input MessagePort entry",
            "real_host_acceptance":"not_measured","model_inference":"not_measured"})
    }
}
impl CommitObserver for Capture {
    fn committed(&self, request: Uuid, head: Sequence) {
        let now = Instant::now();
        let mut d = self.data.lock().unwrap();
        if d.requests.remove(&request) {
            // Tracked Post allocates its Header immediately before its audit.
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
    pub generations: HashMap<Uuid, Uuid>,
}
enum Kind {
    Message { addressed: bool },
    Notice,
}
impl SyntheticPort {
    fn claim_attempt(&self, attempt: u32) {
        // Definite pre-entry refusals consume a claim attempt without native
        // acceptance. record() separately rejects every repeated ID/sequence.
        assert!((1..=3).contains(&attempt));
        let mut data = self.capture.data.lock().unwrap();
        data.max_claim_attempt = data.max_claim_attempt.max(attempt);
    }

    fn record(&self, c: &Context, id: Uuid, seq: i64, bytes: usize, kind: Kind, entry: Instant) {
        assert_eq!(c.host_version, "fixture-only");
        assert_eq!(c.os_profile, "synthetic-not-admitted");
        let mut d = self.capture.data.lock().unwrap();
        if let Some(last) = d.last_sequences.insert(c.context_id, seq) {
            assert!(seq > last, "native FIFO reordered or repeated input");
        }
        if matches!(kind, Kind::Message { .. }) {
            assert!(d.body_deliveries.insert(id));
        } else {
            assert!(d.notice_deliveries.insert(id));
        }
        d.presentations += 1;
        d.max_bytes = d.max_bytes.max(bytes);
        if matches!(kind, Kind::Message { addressed: true }) {
            assert!(d.dispatched.insert(seq));
            let commit = d
                .commits
                .remove(&seq)
                .expect("committed before native scheduling");
            d.latency_ms
                .push(entry.duration_since(commit).as_secs_f64() * 1000.0);
        }
        let route = &self.routes[&c.context_id];
        d.max_model_queue = d.max_model_queue.max(4 - route.capacity());
    }
}
#[async_trait]
impl MessagePort for SyntheticPort {
    async fn present_message(&self, c: &Context, d: &MessageDispatch) -> Result<MessageProgress> {
        let entry = Instant::now();
        assert_eq!(d.input.generation, self.generations[&c.context_id]);
        assert!(!d.awaiting_acceptance);
        self.claim_attempt(d.attempt);
        let text = d.input.text().unwrap();
        let input = MessageInput::from_text(&text).unwrap();
        assert_eq!(input, d.input);
        self.record(
            c,
            input.delivery_id,
            input.seq.get(),
            text.len(),
            Kind::Message {
                addressed: input.addressed,
            },
            entry,
        );
        self.routes[&c.context_id]
            .send(Input::Body(Box::new(input)))
            .await
            .unwrap();
        Ok(MessageProgress::Accepted(HostReceipt::default()))
    }
    async fn present_notice(&self, c: &Context, d: &NoticeDispatch) -> Result<MessageProgress> {
        let entry = Instant::now();
        assert_eq!(d.input.generation, self.generations[&c.context_id]);
        assert!(!d.awaiting_acceptance);
        self.claim_attempt(d.attempt);
        let text = d.input.text().unwrap();
        assert_eq!(NoticeInput::from_text(&text).unwrap(), d.input);
        self.record(
            c,
            d.input.delivery_id,
            d.input.dispatch.creation_seq.get(),
            text.len(),
            Kind::Notice,
            entry,
        );
        Ok(MessageProgress::Accepted(HostReceipt::default()))
    }
}
