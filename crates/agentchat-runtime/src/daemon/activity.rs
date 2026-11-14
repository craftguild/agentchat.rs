//! RAII activity accounting; an unbound connection is not a participant.

use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};

#[derive(Clone)]
pub(super) struct Activity(Arc<Mutex<State>>);
struct State {
    requests: usize,
    observers: usize,
    participants: usize,
    last: Instant,
}
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Request,
    Observer,
}
pub(super) struct Guard {
    activity: Activity,
    kind: Kind,
}
impl Activity {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            requests: 0,
            observers: 0,
            participants: 0,
            last: Instant::now(),
        })))
    }
    pub fn enter(&self, kind: Kind) -> Guard {
        let mut state = self
            .0
            .lock()
            .expect("activity mutex is never held across user code");
        match kind {
            Kind::Request => state.requests += 1,
            Kind::Observer => state.observers += 1,
        }
        state.last = Instant::now();
        Guard {
            activity: self.clone(),
            kind,
        }
    }
    pub fn participants(&self, count: usize) {
        let mut state = self.0.lock().expect("activity state");
        if count > 0 || state.participants > 0 {
            state.last = Instant::now();
        }
        state.participants = count;
    }
    pub fn expired(&self, idle: Duration) -> bool {
        let state = self.0.lock().expect("activity state");
        state.requests == 0
            && state.observers == 0
            && state.participants == 0
            && state.last.elapsed() >= idle
    }
    pub fn status(&self) -> agentchat_protocol::health::ActivityHealth {
        let state = self.0.lock().expect("activity state");
        agentchat_protocol::health::ActivityHealth {
            requests: state.requests,
            observers: state.observers,
            attached_connections: state.participants,
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let mut state = self.activity.0.lock().expect("activity state");
        match self.kind {
            Kind::Request => state.requests -= 1,
            Kind::Observer => state.observers -= 1,
        }
        state.last = Instant::now();
    }
}
