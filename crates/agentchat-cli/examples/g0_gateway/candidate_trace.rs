//! Bounded monotonic G0 timing; never native eligibility or delivery authority.
use agentchat_core::{delivery::WakeDispatch, sequence::Sequence};
use agentchat_runtime::daemon::CommitObserver;
use std::{collections::VecDeque, sync::Mutex, time::Instant};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct Timing {
    commits: Mutex<VecDeque<(Sequence, Instant)>>,
}
impl CommitObserver for Timing {
    fn committed(&self, _request: Uuid, head: Sequence) {
        let now = Instant::now();
        let mut commits = self.commits.lock().expect("candidate timing lock");
        if commits.len() == 64 {
            commits.pop_front();
        }
        commits.push_back((head, now));
    }
}
impl Timing {
    pub fn entry(&self, dispatch: &WakeDispatch, entered: Instant) -> serde_json::Value {
        let commits = self.commits.lock().expect("candidate timing lock");
        let samples = dispatch
            .hint
            .events
            .iter()
            .map(|event| {
                let elapsed = commits
                    .iter()
                    .find(|(head, _)| *head == event.seq)
                    .map(|(_, at)| entered.saturating_duration_since(*at).as_secs_f64() * 1000.0);
                serde_json::json!({"seq":event.seq,"commit_return_to_port_entry_ms":elapsed})
            })
            .collect::<Vec<_>>();
        serde_json::json!({"event":"candidate_port_entry","token":dispatch.token,"context_id":dispatch.hint.context_id,"samples":samples})
    }
}
