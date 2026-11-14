//! Fixed-recipient post-commit observations and independent bounded drain guard.
use agentchat_core::{delivery::NativeDispatch, ports::Storage, sequence::Cursor};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use uuid::Uuid;

pub const STALL: Duration = Duration::from_secs(30);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub delivery: Uuid,
    pub token: Uuid,
    pub generation: Uuid,
    pub attempt: u32,
}
impl Claim {
    fn fingerprint(&self) -> (Uuid, Uuid, Uuid, u32) {
        (self.delivery, self.token, self.generation, self.attempt)
    }
}
#[derive(Clone, Debug)]
pub struct Participant {
    pub native: u64,
    pub seq: i64,
    pub read: usize,
    pub positive_at: Instant,
    pub stage: &'static str,
    pub claim: Option<Claim>,
    pub generation: Option<Uuid>,
}
/// One recipient's transient observations; callbacks never lock the registry.
pub struct ReceiverSlot {
    pub(super) state: Mutex<Participant>,
    initial: Participant,
    changed: Arc<Notify>,
}
impl ReceiverSlot {
    pub fn stage(&self, stage: &'static str) {
        self.state.lock().unwrap().stage = stage;
    }
    pub fn prepared(&self, dispatch: &NativeDispatch) {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.generation, Some(dispatch.generation()));
        state.stage = "native_finish";
        state.claim = Some(Claim {
            delivery: dispatch.delivery_id(),
            token: dispatch.token(),
            generation: dispatch.generation(),
            attempt: dispatch.attempt(),
        });
    }
    pub fn native(&self, dispatch: &NativeDispatch) {
        {
            let mut state = self.state.lock().unwrap();
            assert_eq!(state.generation, Some(dispatch.generation()));
            assert!(dispatch.seq().get() > state.seq);
            state.seq = dispatch.seq().get();
            state.native += 1;
            state.positive_at = Instant::now();
            state.claim = None;
        }
        // Wake only for committed facts and never under an observation lock.
        self.changed.notify_one();
    }
    pub fn read(&self, count: usize) {
        {
            let mut state = self.state.lock().unwrap();
            assert!(count > state.read);
            state.read = count;
            state.positive_at = Instant::now();
        }
        self.changed.notify_one();
    }
}
pub struct Progress {
    slots: Mutex<BTreeMap<Uuid, Arc<ReceiverSlot>>>,
    cache: Mutex<BTreeMap<Uuid, Participant>>,
    changed: Arc<Notify>,
    main_stage: Mutex<(&'static str, Option<Uuid>)>,
    conditions: Mutex<String>,
}
impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            slots: Mutex::new(BTreeMap::new()),
            cache: Mutex::new(BTreeMap::new()),
            changed: Arc::new(Notify::new()),
            main_stage: Mutex::new(("drain_start", None)),
            conditions: Mutex::new("not_yet_observed".into()),
        })
    }
    pub fn register(&self, agent: Uuid, generation: Uuid) -> Arc<ReceiverSlot> {
        let initial = Participant {
            native: 0,
            seq: 0,
            read: 0,
            positive_at: Instant::now(),
            stage: "receiving",
            claim: None,
            generation: Some(generation),
        };
        let slot = Arc::new(ReceiverSlot {
            state: Mutex::new(initial.clone()),
            initial,
            changed: self.changed.clone(),
        });
        assert!(
            self.slots
                .lock()
                .unwrap()
                .insert(agent, slot.clone())
                .is_none()
        );
        slot
    }
    pub fn snapshot(&self) -> BTreeMap<Uuid, Participant> {
        // Drop registry lock before touching individual slots. Never wait on
        // a receiver: unavailable data cannot advance its clock or claim.
        let slots = self.slots.lock().unwrap().clone();
        let mut cache = self.cache.lock().unwrap();
        for (agent, slot) in slots {
            match slot.state.try_lock() {
                Ok(state) => {
                    cache.insert(agent, state.clone());
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    cache.entry(agent).or_insert_with(|| slot.initial.clone());
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    panic!("recipient {agent} observation poisoned")
                }
            }
        }
        cache.clone()
    }
    pub fn main_stage(&self, stage: &'static str, context: Option<Uuid>) {
        *self.main_stage.lock().unwrap() = (stage, context);
    }
    pub fn conditions(
        &self,
        read: usize,
        pending: Option<u64>,
        due: Option<usize>,
        timeouts: usize,
    ) {
        *self.conditions.lock().unwrap() = format!(
            "last_observed fully_read={read} pending={pending:?} due={due:?} timeout_count={timeouts}"
        );
    }
}

#[derive(Clone, Debug, Default)]
pub struct QueueProof {
    pub empty: bool,
    pub head: Option<Claim>,
    pub generation: Option<Uuid>,
}
#[derive(Clone, Debug)]
struct Clock {
    native: u64,
    read: usize,
    last: Instant,
    done: bool,
}
pub struct Guard {
    started: Instant,
    absolute: bool,
    goal: usize,
    clocks: BTreeMap<Uuid, Clock>,
    control_last: Instant,
}
impl Guard {
    pub fn new(
        now: Instant,
        absolute: bool,
        goal: usize,
        states: &BTreeMap<Uuid, Participant>,
    ) -> Self {
        Self {
            started: now,
            absolute,
            goal,
            control_last: now,
            clocks: states
                .iter()
                .map(|(id, s)| {
                    (
                        *id,
                        Clock {
                            native: s.native,
                            read: s.read,
                            last: now,
                            done: false,
                        },
                    )
                })
                .collect(),
        }
    }
    pub fn deadline(&self) -> Instant {
        let recipient = self
            .clocks
            .values()
            .filter(|c| !c.done)
            .map(|c| c.last + STALL)
            .min();
        if self.absolute {
            recipient
                .unwrap_or(self.started + STALL)
                .min(self.started + STALL)
        } else {
            recipient.unwrap_or(self.control_last + STALL)
        }
    }
    pub fn all_done(&self) -> bool {
        self.clocks.values().all(|clock| clock.done)
    }
    pub fn check(
        &mut self,
        now: Instant,
        states: &BTreeMap<Uuid, Participant>,
        proofs: &BTreeMap<Uuid, QueueProof>,
    ) -> Result<(), String> {
        if self.absolute && now.duration_since(self.started) >= STALL {
            return Err(format!(
                "absolute30s drain deadline: states={states:?}; queues={proofs:?}"
            ));
        }
        for (agent, clock) in &mut self.clocks {
            let state = &states[agent];
            if let (Some(original), Some(current)) = (
                state.generation,
                proofs.get(agent).and_then(|p| p.generation),
            ) && original != current
            {
                return Err(format!(
                    "recipient {agent} original generation changed: {original} -> {current}"
                ));
            }
            assert!(
                state.native >= clock.native && state.read >= clock.read && state.read <= self.goal
            );
            if state.native > clock.native || state.read > clock.read {
                self.control_last = self.control_last.max(state.positive_at);
            }
            let done = state.read == self.goal
                && state.claim.is_none()
                && proofs
                    .get(agent)
                    .is_some_and(|p| p.empty && p.head.is_none());
            if done {
                clock.done = true;
                clock.native = state.native;
                clock.read = state.read;
                continue;
            }
            if clock.done {
                clock.last = now;
                clock.done = false;
            }
            if state.native > clock.native || state.read > clock.read {
                clock.last = clock.last.max(state.positive_at);
                clock.native = state.native;
                clock.read = state.read;
            }
            if now.duration_since(clock.last) >= STALL {
                let retained = proofs
                    .get(agent)
                    .and_then(|p| p.head.as_ref())
                    .map(Claim::fingerprint);
                return Err(format!(
                    "recipient {agent} no durable progress30s: {state:?}; queue={:?}; original={retained:?}",
                    proofs.get(agent)
                ));
            }
        }
        if self.all_done() && now.duration_since(self.control_last) >= STALL {
            return Err(format!(
                "control wait30s after last durable progress: states={states:?}; queues={proofs:?}"
            ));
        }
        Ok(())
    }
}

async fn queue_proofs(storage: &dyn Storage, agents: &[Uuid]) -> BTreeMap<Uuid, QueueProof> {
    let mut snapshot = storage.snapshot().await.unwrap();
    let mut result = BTreeMap::new();
    for &agent in agents {
        let empty = snapshot
            .inbox(agent, Cursor::START, 1)
            .await
            .unwrap()
            .is_empty();
        let launch = snapshot
            .latest_launch(agent)
            .await
            .unwrap()
            .expect("original test launch");
        let stream = snapshot
            .message_stream(launch.identity.generation)
            .await
            .unwrap()
            .expect("original test stream");
        let head = stream.head.map(|h| Claim {
            delivery: h.delivery_id,
            token: h.token,
            generation: stream.generation,
            attempt: h.attempts,
        });
        result.insert(
            agent,
            QueueProof {
                empty,
                head,
                generation: Some(stream.generation),
            },
        );
    }
    result
}

/// No I/O await occupies the guard branch: Flush and RO sampling are polled
/// alongside the same per-recipient deadline, even when neither can reply.
pub async fn monitor<F: Future<Output = ()>>(
    progress: Arc<Progress>,
    storage: &dyn Storage,
    goal: usize,
    absolute: bool,
    drain: F,
) {
    let states = progress.snapshot();
    let guard = Guard::new(Instant::now(), absolute, goal, &states);
    monitor_guarded(progress, storage, guard, BTreeMap::new(), drain)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
}

/// The explicit guard constructor permits finite clock-boundary regressions;
/// ordinary workload callers always start at their actual drain boundary.
pub async fn monitor_guarded<F: Future<Output = ()>>(
    progress: Arc<Progress>,
    storage: &dyn Storage,
    mut guard: Guard,
    mut latest: BTreeMap<Uuid, QueueProof>,
    drain: F,
) -> Result<(), String> {
    let agents = progress.snapshot().keys().copied().collect::<Vec<_>>();
    let mut sample = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::pin!(drain);
    let mut drain_done = false;
    let mut last_report = Instant::now();
    loop {
        let deadline = tokio::time::Instant::from_std(guard.deadline());
        tokio::select! {
            () = &mut drain, if !drain_done => {
                drain_done = true;
                progress.main_stage("verify_empty_original_head", None);
                latest.clear();
                sample = Some(Box::pin(queue_proofs(storage, &agents)));
            },
            _ = progress.changed.notified() => {},
            _ = tick.tick() => { if sample.is_none() { sample = Some(Box::pin(queue_proofs(storage, &agents))); } },
            proof = async { sample.as_mut().expect("guarded sample").await }, if sample.is_some() => {
                latest = proof; sample = None;
            },
            _ = tokio::time::sleep_until(deadline) => {},
        }
        let states = progress.snapshot();
        let main = *progress.main_stage.lock().unwrap();
        let conditions = progress.conditions.lock().unwrap().clone();
        if last_report.elapsed() >= STALL {
            eprintln!(
                "WORKLOAD_DRAIN_PROGRESS main={main:?} {conditions} recipients={states:?} queues={latest:?}"
            );
            last_report = Instant::now();
        }
        guard
            .check(Instant::now(), &states, &latest)
            .map_err(|error| format!("{error}; main={main:?}; {conditions}"))?;
        if drain_done && guard.all_done() {
            return Ok(());
        }
    }
}
