//! Bounded native preparation/I/O jobs; claim policy stays in the application.
#[path = "delivery/input.rs"]
pub(super) mod input;
use crate::diagnostics::Sink;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::Application,
    delivery::{
        DeliveryPort, MAX_WAKE_COMPLETION_GROUP, MAX_WAKE_SCAN_CONTEXTS, MessageCompletion,
        MessagePort, MessageProgress, NativeCompletion, SUBMISSION_SECONDS,
    },
    model::Context,
    sequence::Cursor,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle, time::timeout};
use uuid::Uuid;

#[derive(Clone)]
pub(super) enum DispatchPort {
    Hint(Arc<dyn DeliveryPort>),
    Message(Arc<dyn MessagePort>),
    Native(Arc<dyn MessagePort>),
}
pub(super) enum Finished {
    Hint(agentchat_core::delivery::WakeCompletion),
    Message(MessageCompletion),
    Native(NativeCompletion),
}
impl Finished {
    pub fn accepted_input(&self) -> Option<(Uuid, agentchat_core::sequence::Sequence, bool)> {
        match self {
            Self::Message(c) if matches!(&c.outcome, Ok(MessageProgress::Accepted(_))) => Some((
                c.dispatch.input.delivery_id,
                c.dispatch.input.seq,
                c.dispatch.awaiting_acceptance,
            )),
            Self::Native(c) if matches!(&c.outcome, Ok(MessageProgress::Accepted(_))) => Some((
                c.dispatch.delivery_id(),
                c.dispatch.seq(),
                c.dispatch.awaiting_acceptance(),
            )),
            _ => None,
        }
    }
    pub fn request(self) -> agentchat_core::application::CommitRequest {
        match self {
            Self::Hint(c) => agentchat_core::application::CommitRequest::WakeCompletion(c),
            Self::Message(c) => agentchat_core::application::CommitRequest::MessageCompletion(c),
            Self::Native(c) => agentchat_core::application::CommitRequest::NativeCompletion(c),
        }
    }
}
pub(super) struct Outcome {
    job: Uuid,
    context: Uuid,
    result: Result<Option<Finished>>,
}
pub(super) struct Dispatches {
    diagnostics: Sink,
    port: Option<DispatchPort>,
    preparation: Option<Arc<dyn input::Preparation>>,
    jobs: HashMap<Uuid, (Uuid, JoinHandle<()>, Cursor)>,
    scan_after: Option<Uuid>,
    sender: mpsc::Sender<Outcome>,
    pub receiver: mpsc::Receiver<Outcome>,
    completion_groups: u64,
    largest_completion_group: usize,
    candidate_scans: u64,
    candidate_contexts: u64,
    candidate_pruned: u64,
    candidate_fallbacks: u64,
}
impl Dispatches {
    pub fn status(&self) -> agentchat_protocol::health::NativeHealth {
        agentchat_protocol::health::NativeHealth {
            jobs: self.jobs.len(),
            max_jobs: MAX_WAKE_SCAN_CONTEXTS,
            queued_completions: self.receiver.len(),
            completion_capacity: 32,
            completion_groups: self.completion_groups,
            largest_completion_group: self.largest_completion_group,
            max_completion_group: MAX_WAKE_COMPLETION_GROUP,
            candidate_scans: self.candidate_scans,
            candidate_contexts: self.candidate_contexts,
            candidate_pruned: self.candidate_pruned,
            candidate_fallbacks: self.candidate_fallbacks,
            max_candidate_contexts: MAX_WAKE_SCAN_CONTEXTS,
        }
    }
    pub fn new(port: Option<DispatchPort>, diagnostics: Sink) -> Self {
        let (sender, receiver) = mpsc::channel(32);
        Self {
            diagnostics,
            port,
            preparation: None,
            jobs: HashMap::new(),
            scan_after: None,
            sender,
            receiver,
            completion_groups: 0,
            largest_completion_group: 0,
            candidate_scans: 0,
            candidate_contexts: 0,
            candidate_pruned: 0,
            candidate_fallbacks: 0,
        }
    }
    pub fn with_preparation(mut self, preparation: Arc<dyn input::Preparation>) -> Self {
        self.preparation = Some(preparation);
        self
    }
    pub fn cancel(&mut self, context: Uuid) {
        if let Some((_, job, _)) = self.jobs.remove(&context) {
            job.abort();
        }
    }
    #[cfg(test)]
    pub async fn start(&mut self, app: &Arc<Application>, contexts: Vec<Context>) -> Result<()> {
        self.start_at(app, contexts, app.event_head().await?).await
    }
    /// Capture the already-published durable revision, without a new query.
    pub async fn start_at(
        &mut self,
        app: &Arc<Application>,
        contexts: Vec<Context>,
        head: Cursor,
    ) -> Result<()> {
        let Some(port) = self.port.clone() else {
            return Ok(());
        };
        self.jobs.retain(|_, (_, job, _)| !job.is_finished());
        let mut seen = HashSet::new();
        let mut contexts = contexts
            .into_iter()
            .filter(|context| {
                seen.insert(context.context_id) && !self.jobs.contains_key(&context.context_id)
            })
            .collect::<Vec<_>>();
        if contexts.is_empty() || self.jobs.len() >= MAX_WAKE_SCAN_CONTEXTS {
            return Ok(());
        }
        // Failed or empty preparations also occupy bounded slots. Rotate the
        // next scan so repeatedly rejected roots cannot starve later recipients.
        // This cursor is scheduling state only, never an eligibility cache.
        contexts.sort_by_key(|context| context.context_id);
        if let Some(previous) = self.scan_after {
            let next = contexts.partition_point(|context| context.context_id <= previous);
            if next < contexts.len() {
                contexts.rotate_left(next);
            }
        }
        let mut failure = None;
        self.candidate_scans = self.candidate_scans.saturating_add(1);
        self.candidate_contexts = self
            .candidate_contexts
            .saturating_add(contexts.len() as u64);
        let candidates = if matches!(&port, DispatchPort::Message(_) | DispatchPort::Native(_)) {
            // The legacy candidate optimization only selects targeted hints.
            // Passive body work must never be pruned by that older predicate.
            // Keep the same rotating, bounded job slots and no negative cache.
            Some(contexts.iter().map(|c| c.context_id).collect())
        } else {
            match timeout(
                Duration::from_secs(SUBMISSION_SECONDS),
                app.wake_candidates(&contexts),
            )
            .await
            .unwrap_or_else(|_| {
                Err(Error::new(
                    ErrorCode::RuntimeUnavailable,
                    "Wake candidate scan exceeded its bounded deadline.",
                ))
            }) {
                Ok(candidates) => Some(candidates),
                Err(error) => {
                    // This read is only an optimization. An error must not hide
                    // independent recipients or change the original claim policy.
                    self.candidate_fallbacks = self.candidate_fallbacks.saturating_add(1);
                    failure = Some(error);
                    None
                }
            }
        };
        for context in contexts {
            if self.jobs.len() >= MAX_WAKE_SCAN_CONTEXTS {
                break;
            }
            if candidates
                .as_ref()
                .is_some_and(|ids| !ids.contains(&context.context_id))
            {
                self.candidate_pruned = self.candidate_pruned.saturating_add(1);
                continue;
            }
            let id = context.context_id;
            let job_id = Uuid::new_v4();
            let app = app.clone();
            let port = port.clone();
            let sender = self.sender.clone();
            let diagnostics = self.diagnostics.clone();
            let preparation = self.preparation.clone();
            let job = tokio::spawn(async move {
                // Exact host verification and the ordinary individual FULL
                // claim remain application policy. Waiting for either must not
                // monopolize the RPC coordinator or an independent reader.
                let result = match &port {
                    DispatchPort::Hint(port) => {
                        prepare_and_present(&app, port.as_ref(), context).await
                    }
                    DispatchPort::Message(port) => {
                        input::message(&app, port.as_ref(), context, &diagnostics).await
                    }
                    DispatchPort::Native(port) => match preparation {
                        Some(preparation) => {
                            input::native(
                                &app,
                                preparation.as_ref(),
                                port.as_ref(),
                                context,
                                &diagnostics,
                            )
                            .await
                        }
                        None => Err(Error::new(
                            ErrorCode::IntegrityError,
                            "Native dispatcher has no owner-side preparation service.",
                        )),
                    },
                };
                let _ = sender
                    .send(Outcome {
                        job: job_id,
                        context: id,
                        result,
                    })
                    .await;
            });
            self.jobs.insert(id, (job_id, job, head));
            self.scan_after = Some(id);
        }
        failure.map_or(Ok(()), Err)
    }
    #[cfg(test)]
    pub fn take_ready(
        &mut self,
        first: Option<Outcome>,
        limit: usize,
    ) -> (Vec<Finished>, Option<Error>, bool) {
        let (finished, failure, completed, _) = self.take_ready_with_empty(first, limit);
        (finished, failure, completed)
    }
    pub fn take_ready_with_empty(
        &mut self,
        first: Option<Outcome>,
        limit: usize,
    ) -> (Vec<Finished>, Option<Error>, bool, Option<Cursor>) {
        let mut group = first.into_iter().collect::<Vec<_>>();
        // Consume only ready callbacks; the coordinator may use remaining group
        // capacity for adjacent mutations, but never waits or crosses a fence.
        while group.len() < limit.min(MAX_WAKE_COMPLETION_GROUP) {
            match self.receiver.try_recv() {
                Ok(finished) => group.push(finished),
                Err(_) => break,
            }
        }
        let completed_job = !group.is_empty();
        let mut completions = Vec::new();
        let mut failure = None;
        let mut empty_at: Option<Cursor> = None;
        for outcome in group {
            let original = self
                .jobs
                .get(&outcome.context)
                .and_then(|(job, _, head)| (*job == outcome.job).then_some(*head));
            if original.is_some() {
                self.jobs.remove(&outcome.context);
            }
            match outcome.result {
                Ok(Some(finished)) => completions.push(finished),
                Ok(None) => {
                    if let Some(head) = original {
                        empty_at = Some(empty_at.map_or(head, |prior| prior.min(head)));
                    }
                }
                // SessionRequired before a Finished value means eligibility
                // disappeared before the application returned a claim and
                // before native I/O began. Lifecycle reconciliation owns any
                // retirement; this is not a transport/control failure.
                Err(error) if error.code == ErrorCode::SessionRequired => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        if !completions.is_empty() {
            self.completion_groups = self.completion_groups.saturating_add(1);
            self.largest_completion_group = self.largest_completion_group.max(completions.len());
        }
        // An outer failure retains the original durable claims/orphan deadline.
        // No new preparation runs until recording this group has returned.
        (completions, failure, completed_job, empty_at)
    }
}
impl Drop for Dispatches {
    fn drop(&mut self) {
        for (_, job, _) in self.jobs.values() {
            job.abort();
        }
    }
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::RuntimeUnavailable,
        "Native presentation exceeded its bounded deadline.",
    )
}

async fn prepare_and_present(
    app: &Application,
    port: &dyn DeliveryPort,
    context: Context,
) -> Result<Option<Finished>> {
    let prepared = timeout(
        Duration::from_secs(SUBMISSION_SECONDS),
        app.prepare_wake(&context),
    )
    .await
    .unwrap_or_else(|_| {
        Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "Native preparation exceeded its bounded deadline.",
        ))
    })?;
    let Some(dispatch) = prepared else {
        return Ok(None);
    };
    // No native effect can start before prepare_wake returns from its durable
    // commit. Cancellation after an ambiguous commit retains its orphan deadline.
    let outcome = timeout(
        Duration::from_secs(SUBMISSION_SECONDS),
        port.present(&context, &dispatch),
    )
    .await
    .unwrap_or_else(|_| Err(unavailable()));
    Ok(Some(Finished::Hint(
        agentchat_core::delivery::WakeCompletion {
            context,
            dispatch,
            outcome,
        },
    )))
}

#[cfg(test)]
impl Dispatches {
    pub(super) fn test_job(&self, context: Uuid) -> Option<Uuid> {
        self.jobs.get(&context).map(|(id, _, _)| *id)
    }
}
#[cfg(test)]
impl Outcome {
    pub(super) fn test_empty(job: Uuid, context: Uuid) -> Self {
        Self {
            job,
            context,
            result: Ok(None),
        }
    }
    pub(super) fn test_with_job(mut self, job: Uuid) -> Self {
        self.job = job;
        self
    }
}
