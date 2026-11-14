//! One bounded blocking maintenance job, driven by the existing gateway tick.
use agentchat_hosts::claude::namespace::OwnerReaper;
use std::path::Path;
use tokio::task::JoinHandle;

pub(super) struct Maintenance {
    idle: Option<OwnerReaper>,
    running: Option<JoinHandle<OwnerReaper>>,
}
impl Maintenance {
    pub fn new(home: &Path) -> Self {
        Self {
            idle: Some(OwnerReaper::new(home).with_context_validator(
                agentchat_runtime::context::ContextRegistry::validate_stage,
            )),
            running: None,
        }
    }
    pub async fn tick(&mut self) {
        if self.running.as_ref().is_some_and(JoinHandle::is_finished) {
            let task = self.running.take().expect("finished maintenance task");
            self.idle = task.await.ok();
        }
        if let Some(mut reaper) = self.idle.take() {
            // Errors preserve private state. They never alter native proof,
            // presence, attachments, request results or host controls.
            self.running = Some(tokio::task::spawn_blocking(move || {
                let _ = reaper.step();
                reaper
            }));
        }
    }
}
