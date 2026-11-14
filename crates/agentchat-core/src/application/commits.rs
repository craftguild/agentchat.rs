//! Ready mutations and native callbacks share durability, not business policy.

use super::{
    Application, ClaimedNative, Committed, MutationRequest, NativeClaim, NativeProcessing,
    ProcessingReady, delivery::reconcile, mutation,
};
use crate::{
    Error, ErrorCode, Result,
    delivery::{MessageCompletion, NativeCompletion, WakeCompletion},
    ports::Transaction,
};

/// Maximum already-ready writes in one immediate FULL transaction.
pub const MAX_COMMIT_GROUP: usize = 8;

/// One application write, retaining its independent actor or exact native claim.
#[derive(Debug, Clone)]
pub enum CommitRequest {
    /// A normalized semantic mutation with its original successful-retry UUID.
    Mutation(MutationRequest),
    /// Start original processing bookkeeping before host-state observation.
    NativeProcessing(NativeProcessing),
    /// Reserve an independently observed original head before native I/O.
    NativeClaim(NativeClaim),
    /// An outcome for a previously committed native presentation attempt.
    WakeCompletion(WakeCompletion),
    /// An outcome for the exact current full-body stream head.
    MessageCompletion(MessageCompletion),
    /// An outcome for the exact current mixed native-input stream head.
    NativeCompletion(NativeCompletion),
}

/// A provisional result made public only after the outer commit succeeds.
#[derive(Debug)]
pub enum CommitOutcome {
    /// The mutation's original result and optional newly committed event head.
    Mutation(Committed),
    /// Processing has returned from the outer commit/no-write rollback boundary.
    NativeProcessing(ProcessingReady),
    /// The original native reservation has returned from its FULL boundary.
    NativeClaim(ClaimedNative),
    /// Whether an exact current native callback changed its retained batch.
    WakeCompletion(bool),
    /// Whether a current full-body head accepted this outcome.
    MessageCompletion(bool),
    /// Whether a current mixed native-input head accepted this outcome.
    NativeCompletion(bool),
}

impl CommitOutcome {
    /// Changed preparation facts, available only after the outer boundary returns.
    pub fn native_preparation_changed(&self) -> Option<bool> {
        match self {
            Self::NativeProcessing(v) => Some(v.changed()),
            Self::NativeClaim(v) => Some(v.changed()),
            _ => None,
        }
    }

    fn no_write_preparation(&self) -> bool {
        match self {
            Self::NativeProcessing(value) => !value.changed(),
            Self::NativeClaim(value) => !value.changed(),
            _ => false,
        }
    }
    pub(crate) fn message_completion(self) -> Result<bool> {
        match self {
            Self::MessageCompletion(recorded) => Ok(recorded),
            _ => unreachable!("message-completion-only commit"),
        }
    }
}

enum Prepared {
    NativeProcessing(NativeProcessing),
    NativeClaim(NativeClaim),
    Mutation(mutation::Prepared),
    WakeCompletion(WakeCompletion),
    MessageCompletion(MessageCompletion),
    NativeCompletion(NativeCompletion),
}
struct Failure {
    error: Error,
    infrastructure: bool,
}

impl Application {
    /// Commit one through eight already-ready writes in the supplied order.
    ///
    /// Each write invokes the ordinary mutation or exact-attempt completion
    /// policy. Independent semantic failures roll back their savepoint; a
    /// storage/savepoint/outer-commit failure aborts all provisional results.
    /// No result, observer hint or new native I/O may precede this return.
    /// An ambiguous commit is recovered using the original mutation UUIDs and
    /// native claims. Callers preserve FIFO query/control fences and add no
    /// collection timer. Captured native facts still require fresh authority and
    /// the exact P selection; this writer never performs host observation.
    pub async fn commit_ready(
        &self,
        requests: Vec<CommitRequest>,
    ) -> Result<Vec<Result<CommitOutcome>>> {
        if requests.is_empty() || requests.len() > MAX_COMMIT_GROUP {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Commit group must contain one through eight ready writes.",
            ));
        }
        let has_existing = requests.iter().any(|request| {
            !matches!(
                request,
                CommitRequest::NativeProcessing(_) | CommitRequest::NativeClaim(_)
            )
        });
        let mut prepared = requests
            .into_iter()
            .map(|request| match request {
                CommitRequest::NativeProcessing(request) => self
                    .validate_context(request.context())
                    .map(|()| Prepared::NativeProcessing(request)),
                CommitRequest::NativeClaim(request) => self
                    .validate_context(request.context())
                    .map(|()| Prepared::NativeClaim(request)),
                CommitRequest::Mutation(request) => {
                    self.prepare_request(request).map(Prepared::Mutation)
                }
                CommitRequest::WakeCompletion(completion) => self
                    .validate_context(&completion.context)
                    .map(|()| Prepared::WakeCompletion(completion)),
                CommitRequest::MessageCompletion(completion) => self
                    .validate_context(&completion.context)
                    .map(|()| Prepared::MessageCompletion(completion)),
                CommitRequest::NativeCompletion(completion) => self
                    .validate_context(&completion.context)
                    .map(|()| Prepared::NativeCompletion(completion)),
            })
            .collect::<Vec<_>>();
        if prepared.iter().all(Result::is_err) {
            return Ok(prepared
                .into_iter()
                .filter_map(Result::err)
                .map(Err)
                .collect());
        }
        for request in &mut prepared {
            if let Ok(Prepared::Mutation(request)) = request {
                request.capture_state(self.host.as_ref()).await;
            }
        }
        let mut outer = self.storage.begin().await?;
        if prepared.len() == 1 {
            let request = prepared.pop().expect("one valid ready write")?;
            return match self.apply_commit(outer.as_mut(), request).await {
                Ok(result) => {
                    if result.no_write_preparation() {
                        outer.rollback().await?;
                    } else {
                        outer.commit().await?;
                    }
                    Ok(vec![Ok(result)])
                }
                Err(failure) => {
                    outer.rollback().await?;
                    if failure.infrastructure {
                        Err(failure.error)
                    } else {
                        Ok(vec![Err(failure.error)])
                    }
                }
            };
        }
        let mut writes = has_existing;
        let mut results = Vec::with_capacity(prepared.len());
        for request in prepared {
            let request = match request {
                Ok(request) => request,
                Err(error) => {
                    results.push(Err(error));
                    continue;
                }
            };
            let mut child = outer.savepoint().await?;
            match self.apply_commit(child.as_mut(), request).await {
                Ok(result) => {
                    if result.no_write_preparation() {
                        child.rollback().await?;
                    } else {
                        child.commit().await?;
                        writes = true;
                    }
                    results.push(Ok(result));
                }
                Err(failure) => {
                    child.rollback().await?;
                    if failure.infrastructure {
                        return Err(failure.error);
                    }
                    results.push(Err(failure.error));
                }
            }
        }
        if writes {
            outer.commit().await?;
        } else {
            outer.rollback().await?;
        }
        Ok(results)
    }

    async fn apply_commit(
        &self,
        tx: &mut dyn Transaction,
        request: Prepared,
    ) -> std::result::Result<CommitOutcome, Failure> {
        match request {
            Prepared::NativeProcessing(request) => self
                .process_native_in(tx, request)
                .await
                .map(CommitOutcome::NativeProcessing)
                .map_err(preparation_failure),
            Prepared::NativeClaim(request) => self
                .reserve_native_in(tx, request)
                .await
                .map(CommitOutcome::NativeClaim)
                .map_err(preparation_failure),
            Prepared::Mutation(request) => self
                .apply_prepared(tx, request)
                .await
                .map(CommitOutcome::Mutation)
                .map_err(|error| Failure {
                    infrastructure: matches!(
                        error.code,
                        ErrorCode::StorageError | ErrorCode::IoError | ErrorCode::IntegrityError
                    ),
                    error,
                }),
            Prepared::WakeCompletion(completion) => reconcile::finish(
                tx,
                &completion.context,
                &completion.dispatch,
                completion.outcome,
                self.clock.now(),
            )
            .await
            .map(CommitOutcome::WakeCompletion)
            .map_err(|failure| Failure {
                infrastructure: failure.infrastructure(),
                error: failure.into_error(),
            }),
            Prepared::MessageCompletion(completion) => self
                .finish_message_in(
                    tx,
                    &completion.context,
                    &completion.dispatch,
                    completion.outcome,
                )
                .await
                .map(CommitOutcome::MessageCompletion)
                .map_err(|failure| Failure {
                    infrastructure: failure.infrastructure(),
                    error: failure.into_error(),
                }),
            Prepared::NativeCompletion(completion) => self
                .finish_native_in(
                    tx,
                    &completion.context,
                    &completion.dispatch,
                    completion.outcome,
                )
                .await
                .map(CommitOutcome::NativeCompletion)
                .map_err(|failure| Failure {
                    infrastructure: failure.infrastructure(),
                    error: failure.into_error(),
                }),
        }
    }
}

fn preparation_failure(error: Error) -> Failure {
    Failure {
        infrastructure: matches!(
            error.code,
            ErrorCode::StorageError | ErrorCode::IoError | ErrorCode::IntegrityError
        ),
        error,
    }
}
