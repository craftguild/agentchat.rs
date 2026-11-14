//! Transport-neutral grouping: all normalization, rollback and retry policy is core.
use super::{Answer, Worker};
use crate::diagnostics::{Event, NativePhase, Sink, elapsed};
use agentchat_core::{
    Error, ErrorCode, Result,
    application::{
        CommitOutcome, CommitRequest, Committed, MAX_COMMIT_GROUP, MAX_MUTATION_GROUP, Mutation,
        MutationRequest,
    },
    model::Context,
};
use agentchat_protocol::rpc::Call;
use std::{collections::VecDeque, time::Instant};
use tokio::sync::oneshot;
use uuid::Uuid;

pub(super) use super::ingress::{MutationPending, Pending, collect};
struct Timing {
    request_id: Option<Uuid>,
    queue_us: u64,
    started: Instant,
}
impl Timing {
    fn record(self, sink: &Sink) {
        sink.record(Event::RpcTiming {
            request_id: self.request_id,
            queue_us: self.queue_us,
            execution_us: elapsed(self.started),
        });
    }
}
#[derive(Default)]
pub(super) struct Metrics {
    groups: u64,
    batched: u64,
    requests: u64,
    largest: usize,
    ready_groups: u64,
    mixed_groups: u64,
    largest_ready: usize,
}
impl Metrics {
    pub fn status(&self) -> agentchat_protocol::health::MutationHealth {
        agentchat_protocol::health::MutationHealth {
            groups: self.groups,
            batched_groups: self.batched,
            requests: self.requests,
            largest: self.largest,
            limit: MAX_MUTATION_GROUP,
            ready_commit_groups: self.ready_groups,
            mixed_commit_groups: self.mixed_groups,
            largest_commit_group: self.largest_ready,
            max_commit_group: MAX_COMMIT_GROUP,
        }
    }
    fn record(&mut self, size: usize) {
        self.groups = self.groups.saturating_add(1);
        self.batched = self.batched.saturating_add(u64::from(size > 1));
        self.requests = self.requests.saturating_add(size as u64);
        self.largest = self.largest.max(size);
    }
}
impl Worker {
    pub(super) fn publish_mutation(
        &mut self,
        request: Uuid,
        context: &Context,
        leaving: bool,
        committed: Committed,
    ) -> Answer {
        self.publish_committed(request, Some(context), leaving, committed)
    }

    pub(super) fn publish_user_mutation(&mut self, request: Uuid, committed: Committed) -> Answer {
        self.publish_committed(request, None, false, committed)
    }

    fn publish_committed(
        &mut self,
        request: Uuid,
        context: Option<&Context>,
        leaving: bool,
        committed: Committed,
    ) -> Answer {
        if let Some(seq) = committed.event_head {
            self.diagnostics.record(Event::MutationCommitted {
                request_id: request,
                seq,
            });
        }
        if let (Some(observer), Some(head)) = (&self.commits, committed.event_head) {
            observer.committed(request, head);
        }
        if leaving && let Some(context) = context {
            self.deliveries.cancel(context.context_id);
        }
        if let Some(head) = committed.event_head {
            self.heads.send_replace(head.cursor());
        }
        Answer {
            result: committed.result.into(),
            subscription: None,
            bind: context.is_some(),
        }
    }
    pub(super) async fn commit_group(
        &mut self,
        pending: Vec<Pending>,
        completions: Vec<super::super::delivery::Finished>,
        native_first: bool,
    ) -> bool {
        enum Slot {
            Mutation(
                oneshot::Sender<Result<Answer>>,
                Box<Result<(Uuid, Context, bool)>>,
                Timing,
            ),
            Completion,
            Native {
                reply: oneshot::Sender<Result<CommitOutcome>>,
                accepted: Option<(Uuid, agentchat_core::sequence::Sequence, bool)>,
                preparatory: bool,
            },
        }
        let completion_count = completions.len();
        let mut accepted_inputs = completions
            .iter()
            .map(|c| c.accepted_input())
            .collect::<VecDeque<_>>();
        let mut requests = Vec::with_capacity(pending.len() + completion_count);
        let mut slots = Vec::with_capacity(pending.len() + completion_count);
        let mut native = completions
            .into_iter()
            .map(super::super::delivery::Finished::request)
            .collect::<Vec<_>>();
        if native_first {
            requests.append(&mut native);
            slots.extend((0..completion_count).map(|_| Slot::Completion));
        }
        let mut mutation_count = 0;
        for pending in pending {
            let pending = match pending {
                Pending::Mutation(value) => value,
                Pending::Native(value) => {
                    if value.skip_preparation() {
                        let _ = value.reply.send(Err(super::super::unavailable(
                            "Native preparation caller expired.",
                        )));
                        continue;
                    }
                    let accepted = value.request.accepted_input();
                    let preparatory = value.request.preparatory();
                    requests.push(value.request.into_commit());
                    slots.push(Slot::Native {
                        reply: value.reply,
                        accepted,
                        preparatory,
                    });
                    continue;
                }
            };
            let MutationPending {
                call,
                bound,
                reply,
                request_id,
                queued_at,
            } = pending;
            let timing = Timing {
                request_id,
                queue_us: elapsed(queued_at),
                started: Instant::now(),
            };
            let context = self.resolve_context(&call, bound).await;
            let prepared = match (call, context) {
                (
                    Call::Mutate {
                        request_id,
                        command,
                        ..
                    },
                    Ok(Some(context)),
                ) => {
                    let leaving = matches!(&command, Mutation::Leave);
                    requests.push(CommitRequest::Mutation(MutationRequest {
                        request_id,
                        context: context.clone(),
                        command,
                    }));
                    mutation_count += 1;
                    Ok((request_id, context, leaving))
                }
                (_, Err(error)) => Err(error),
                _ => Err(super::session()),
            };
            slots.push(Slot::Mutation(reply, Box::new(prepared), timing));
        }
        if !native_first {
            requests.append(&mut native);
            slots.extend((0..completion_count).map(|_| Slot::Completion));
        }
        let count = requests.len();
        let outcomes = if count == 0 {
            Vec::new()
        } else {
            if mutation_count > 0 {
                self.grouping.record(mutation_count);
            }
            self.grouping.ready_groups = self.grouping.ready_groups.saturating_add(1);
            self.grouping.mixed_groups = self
                .grouping
                .mixed_groups
                .saturating_add(u64::from(mutation_count > 0 && completion_count > 0));
            self.grouping.largest_ready = self.grouping.largest_ready.max(count);
            match self.app.commit_ready(requests).await {
                Ok(results) => results,
                Err(error) => (0..count).map(|_| Err(error.clone())).collect(),
            }
        };
        let mut outcomes = outcomes.into_iter();
        let mut schedule = completion_count > 0;
        let mut native_error = None;
        // The selected branch comes first; the other queue contributes only
        // already-ready writes. No result, observer hint or cancellation escapes
        // until the shared FULL commit returns. Queries/control calls stay fences.
        for slot in slots {
            match slot {
                Slot::Completion => {
                    let accepted = accepted_inputs.pop_front().expect("one trace per callback");
                    match outcomes.next() {
                        Some(Ok(
                            CommitOutcome::MessageCompletion(recorded)
                            | CommitOutcome::NativeCompletion(recorded),
                        )) => {
                            if recorded && let Some((delivery_id, seq, receipt_only)) = accepted {
                                // commit_ready returns only after its FULL outer
                                // commit. Never report provisional/rolled-back settlement.
                                self.diagnostics.record(Event::NativeInput {
                                    delivery_id,
                                    seq,
                                    receipt_only,
                                    phase: NativePhase::AcceptanceCommitted,
                                    duration_us: 0,
                                    outcome: None,
                                });
                            }
                        }
                        Some(Ok(CommitOutcome::WakeCompletion(_))) => {}
                        Some(Err(error)) => {
                            native_error.get_or_insert(error);
                        }
                        _ => {
                            native_error.get_or_insert_with(lost);
                        }
                    }
                }
                Slot::Native {
                    reply,
                    accepted,
                    preparatory,
                } => {
                    let result = outcomes.next().unwrap_or_else(|| Err(lost()));
                    if let Ok(value) = &result {
                        let changed = value.native_preparation_changed().unwrap_or(!preparatory);
                        schedule |= changed;
                        self.native_jobs_completed |= changed;
                        if matches!(value, CommitOutcome::NativeCompletion(true))
                            && let Some((delivery_id, seq, receipt_only)) = accepted
                        {
                            self.diagnostics.record(Event::NativeInput {
                                delivery_id,
                                seq,
                                receipt_only,
                                phase: NativePhase::AcceptanceCommitted,
                                duration_us: 0,
                                outcome: None,
                            });
                        }
                    } else if let Err(error) = &result {
                        // Expired/dropped preparatory admission is local. Actual
                        // completion and infrastructure errors remain diagnostic failures.
                        if !preparatory
                            || !reply.is_closed()
                            || error.code != ErrorCode::RuntimeUnavailable
                        {
                            native_error.get_or_insert(error.clone());
                        }
                    }
                    // An actual positive native completion is applied even if
                    // this receiver was dropped. A lost reply never undoes FULL.
                    let _ = reply.send(result);
                }
                Slot::Mutation(reply, prepared, timing) => {
                    let result = match *prepared {
                        Ok((request, context, leaving)) => match outcomes.next() {
                            Some(Ok(CommitOutcome::Mutation(committed))) => {
                                schedule = true;
                                Ok(self.publish_mutation(request, &context, leaving, committed))
                            }
                            Some(Err(error)) => Err(error),
                            _ => Err(lost()),
                        },
                        Err(error) => Err(error),
                    };
                    self.diagnostics.record(match &result {
                        Ok(_) => Event::RequestSucceeded,
                        Err(error) => Event::RequestFailed { code: error.code },
                    });
                    timing.record(&self.diagnostics);
                    // A lost caller does not undo a committed semantic UUID.
                    let _ = reply.send(result);
                }
            }
        }
        if let Some(error) = native_error {
            self.control_error(Event::NativeFailed { code: error.code }, error.code);
        }
        if std::mem::take(&mut self.native_jobs_completed) {
            // A preparation can commit expiry/departure even when it returns
            // no dispatch; a completion can create the first timeout notice.
            // Read only the durable head after the bounded job/commit group.
            // Failed or rolled-back attempts can never publish a guessed head.
            match self.app.event_head().await {
                Ok(head) => {
                    self.heads.send_if_modified(|current| {
                        if head > *current {
                            *current = head;
                            true
                        } else {
                            false
                        }
                    });
                }
                Err(error) => {
                    self.control_error(Event::NativeFailed { code: error.code }, error.code);
                }
            }
        }
        // An exact empty job can have observed no work before a later durable
        // event arrived. Retry once at the now-current head; this reschedule
        // does not create a revision or change an original native claim.
        if let Some(started_at) = self.native_empty_at.take() {
            schedule |= *self.heads.borrow() > started_at;
        }
        schedule
    }
}
fn lost() -> Error {
    Error::new(
        ErrorCode::IntegrityError,
        "Ready commit group lost an outcome.",
    )
}
