//! Bounded shared durability with isolated per-request rollback and retry keys.
use super::{Application, CommitOutcome, CommitRequest, Committed, Mutation};
use crate::{Error, ErrorCode, Result, model::Context};
use uuid::Uuid;

/// Maximum already-queued mutations sharing one immediate outer transaction.
pub const MAX_MUTATION_GROUP: usize = 8;

/// One exact actor and successful-retry identity, independent of its transport.
#[derive(Debug, Clone)]
pub struct MutationRequest {
    /// Stable semantic UUID, retained with this operation's own successful result.
    pub request_id: Uuid,
    /// Exact project/conversation/incarnation; never inferred from the group.
    pub context: Context,
    /// Concrete domain operation using the ordinary mutation policy.
    pub command: Mutation,
}

impl Application {
    /// Execute one through eight ordered requests at a shared durability boundary.
    ///
    /// Each valid request uses the same policy as `execute`; multi-write groups
    /// isolate each request in its own savepoint.
    /// Domain failures roll back only that request. Later requests observe prior
    /// successful staged effects, including retry keys, identity and leases.
    /// No provisional success escapes until the outer FULL commit succeeds.
    /// Infrastructure/savepoint/commit failures abort the group and return an
    /// outer error; callers must retry original semantic UUIDs after uncertainty.
    /// A cancelled caller cannot infer rollback of an already submitted group.
    /// The returned inner results have exactly the input ordering and length.
    pub async fn execute_group(
        &self,
        requests: Vec<MutationRequest>,
    ) -> Result<Vec<Result<Committed>>> {
        if requests.is_empty() || requests.len() > MAX_MUTATION_GROUP {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Mutation group must contain one through eight requests.",
            ));
        }
        Ok(self
            .commit_ready(requests.into_iter().map(CommitRequest::Mutation).collect())
            .await?
            .into_iter()
            .map(|result| {
                result.map(|outcome| match outcome {
                    CommitOutcome::Mutation(committed) => committed,
                    CommitOutcome::WakeCompletion(_)
                    | CommitOutcome::MessageCompletion(_)
                    | CommitOutcome::NativeCompletion(_)
                    | CommitOutcome::NativeProcessing(_)
                    | CommitOutcome::NativeClaim(_) => {
                        unreachable!("mutation-only commit group")
                    }
                })
            })
            .collect::<Vec<_>>())
    }
}
