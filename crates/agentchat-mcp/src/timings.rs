//! Best-effort bounded transport measurements, never identity or business proof.
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use uuid::Uuid;

const CAPACITY: usize = 32;

#[derive(Clone, Default)]
pub(super) struct Timings(Arc<Inner>);
#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    dropped: AtomicU64,
}
#[derive(Default)]
struct State {
    samples: VecDeque<Sample>,
    overwritten: u64,
}
#[derive(Serialize)]
struct Sample {
    request_id: Uuid,
    verification_us: u64,
    execution_us: u64,
    verified: bool,
}

pub(super) fn elapsed(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

impl Timings {
    pub fn record(&self, id: Uuid, verification_us: u64, execution_us: u64, verified: bool) {
        let Ok(mut state) = self.0.state.try_lock() else {
            let _ = self
                .0
                .dropped
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    Some(value.saturating_add(1))
                });
            return;
        };
        if state.samples.len() == CAPACITY {
            state.samples.pop_front();
            state.overwritten = state.overwritten.saturating_add(1);
        }
        state.samples.push_back(Sample {
            request_id: id,
            verification_us,
            execution_us,
            verified,
        });
    }

    pub fn snapshot(&self) -> Option<Value> {
        let state = self.0.state.try_lock().ok()?;
        if state.samples.is_empty() {
            return None;
        }
        Some(json!({
            "version":1, "clock":"process_local_durations",
            "capacity":CAPACITY, "overwritten":state.overwritten,
            "dropped":self.0.dropped.load(Ordering::Relaxed), "samples":state.samples,
        }))
    }
}
