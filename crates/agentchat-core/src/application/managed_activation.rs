//! Provider-independent verified managed-root activation seam.

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{Application, Work, launch};
use crate::{
    Error, ErrorCode, Result,
    model::{Context, LaunchIdentity, LaunchOwnerState},
    ports::{HostState, IdempotencyRecord},
    sequence::Sequence,
    validation,
};

/// Correlated lifecycle cause for a managed activation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum ManagedActivationCause {
    /// First verified ready root owned by this managed launch.
    Initial,
    /// Successful transition away from one exact active predecessor.
    DistinctRoot {
        /// Previous application context fenced by this transition.
        predecessor_context: Uuid,
        /// Previous delivery generation fenced by this transition.
        predecessor_generation: Uuid,
        /// Monotonic host-adapter transition epoch.
        transition_epoch: u64,
    },
}

/// Internal activation intent; it contains no authority by itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ManagedActivationRequest {
    /// Stable semantic retry identity retained across an unknown reply.
    pub request_id: Uuid,
    /// Exact host-issued target descriptor in the authorized project.
    pub target_context: Context,
    /// Initial or correlated distinct-root activation evidence requested.
    pub cause: ManagedActivationCause,
    /// Optional immutable participant handle.
    pub name: Option<String>,
}

/// Host-verified activation facts. Callers cannot construct this token.
pub struct VerifiedManagedActivation {
    request: ManagedActivationRequest,
    identity: LaunchIdentity,
    state: HostState,
    authorized_project: Uuid,
    original_owner: String,
}

/// Typed durable result of one managed activation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManagedActivationResult {
    /// Retained target participant.
    pub agent_id: Uuid,
    /// Exact activated application context.
    pub context_id: Uuid,
    /// Fresh managed delivery generation.
    pub generation: Uuid,
    /// Forward participation boundary for this activation.
    pub join_boundary: Sequence,
}

impl Application {
    /// Replay a committed managed activation receipt before consulting mutable
    /// host readiness. A matching durable receipt is historical result data, not
    /// renewed activation authority; only the no-receipt path performs fresh
    /// host verification and the ordinary writer-time recheck.
    pub async fn activate_managed_request(
        &self,
        mut request: ManagedActivationRequest,
    ) -> Result<ManagedActivationResult> {
        if request.request_id.is_nil() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Managed activation request ID cannot be nil.",
            ));
        }
        self.validate_context(&request.target_context)?;
        request.name = request
            .name
            .as_deref()
            .map(validation::handle)
            .transpose()?;
        validate_cause(&request)?;
        let hash = argument_hash(&request)?;
        let actor_key = actor_key(&request)?;
        let mut tx = self.storage.begin().await?;
        if let Some(record) = tx.idempotency(request.request_id).await? {
            let result = if record.actor_key != actor_key
                || record.operation != "managed_activation"
                || record.argument_hash != hash
            {
                Err(Error::new(
                    ErrorCode::IdempotencyConflict,
                    "Managed activation request ID already has different arguments.",
                ))
            } else {
                decode_result(&record.result)
            };
            tx.rollback().await?;
            return result;
        }
        tx.rollback().await?;
        let verified = self.verify_managed_activation(request).await?;
        self.activate_managed(verified).await
    }

    /// Capture bounded authoritative target facts before acquiring the writer.
    pub async fn verify_managed_activation(
        &self,
        mut request: ManagedActivationRequest,
    ) -> Result<VerifiedManagedActivation> {
        if request.request_id.is_nil() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Managed activation request ID cannot be nil.",
            ));
        }
        self.validate_context(&request.target_context)?;
        request.name = request
            .name
            .as_deref()
            .map(validation::handle)
            .transpose()?;
        validate_cause(&request)?;
        let identity = self
            .host
            .launch_identity(&request.target_context)
            .await?
            .ok_or_else(|| super::activation::managed_required(&request.target_context))?;
        let state = self.host.verify(&request.target_context).await?;
        if identity.generation.is_nil()
            || identity.owner.is_empty()
            || identity.owner.len() > 32_768
            || self.host.launch_owner(&identity) != LaunchOwnerState::Current
            || self.launch_root_changed(&request.target_context, &identity)
        {
            return Err(super::activation::managed_required(&request.target_context));
        }
        Ok(VerifiedManagedActivation {
            authorized_project: request.target_context.project_id,
            original_owner: identity.owner.clone(),
            request,
            identity,
            state,
        })
    }

    /// Fence a predecessor and commit one verified target activation atomically.
    pub async fn activate_managed(
        &self,
        mut verified: VerifiedManagedActivation,
    ) -> Result<ManagedActivationResult> {
        let request = &verified.request;
        if request.target_context.project_id != verified.authorized_project
            || verified.identity.owner != verified.original_owner
        {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "Verified managed activation facts changed internally.",
            ));
        }
        let current = self
            .host
            .launch_identity(&request.target_context)
            .await?
            .ok_or_else(|| super::activation::managed_required(&request.target_context))?;
        verified.state = self.host.verify(&request.target_context).await?;
        if current != verified.identity
            || self.host.launch_owner(&current) != LaunchOwnerState::Current
            || self.launch_root_changed(&request.target_context, &current)
        {
            return Err(super::activation::managed_required(&request.target_context));
        }
        let hash = argument_hash(request)?;
        let actor_key = actor_key(request)?;
        let mut tx = self.storage.begin().await?;
        if let Some(record) = tx.idempotency(request.request_id).await? {
            if record.actor_key != actor_key
                || record.operation != "managed_activation"
                || record.argument_hash != hash
            {
                return Err(Error::new(
                    ErrorCode::IdempotencyConflict,
                    "Managed activation request ID already has different arguments.",
                ));
            }
            return decode_result(&record.result);
        }
        let now = self.clock.now();
        let mut work = Work {
            tx: tx.as_mut(),
            now,
            last_seq: None,
        };
        if matches!(request.cause, ManagedActivationCause::Initial)
            && let Some(agent) = work
                .tx
                .conversation(
                    request.target_context.host,
                    &request.target_context.host_session_id,
                )
                .await?
        {
            let latest = work.tx.latest_launch(agent.id).await?;
            let exact_active = agent.joined
                && latest.as_ref().is_some_and(|launch| {
                    launch.identity == verified.identity
                        && launch.context_id == request.target_context.context_id
                        && launch.retired_at.is_none()
                });
            if !exact_active && let Some(previous) = latest {
                // A same-physical-owner initial request is a same-root replay,
                // not a distinct-root transition. It must not undo Leave or
                // bypass the correlated predecessor/epoch required on return.
                if previous.identity.owner == verified.original_owner
                    || self.host.launch_owner(&previous.identity) != LaunchOwnerState::Gone
                {
                    return Err(Error::new(
                        ErrorCode::IdentityConflict,
                        "Initial managed activation cannot replace a retained current or unconfirmed root owner.",
                    ));
                }
                if previous.retired_at.is_none() {
                    launch::retire(&mut work, previous, "owner_lost").await?;
                }
            }
        }
        if let ManagedActivationCause::DistinctRoot {
            predecessor_context,
            predecessor_generation,
            ..
        } = request.cause
        {
            let predecessor = work
                .tx
                .launch(predecessor_generation)
                .await?
                .filter(|launch| {
                    // Leave fences this root immediately, but it must not fence
                    // a later correlated root. The exact latest generation,
                    // current predecessor session, original owner and durable
                    // root-departure checks below still authorize that switch.
                    launch.retired_at.is_none()
                        || matches!(
                            launch.retirement_reason.as_deref(),
                            Some("root_changed" | "explicit_leave")
                        )
                })
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::IdentityConflict,
                        "Managed activation predecessor is no longer current.",
                    )
                })?;
            if predecessor.identity.owner != verified.original_owner {
                return Err(Error::new(
                    ErrorCode::IdentityConflict,
                    "Managed activation predecessor has a different original owner.",
                ));
            }
            let predecessor_agent =
                work.tx.agent(predecessor.agent_id).await?.ok_or_else(|| {
                    Error::new(
                        ErrorCode::IntegrityError,
                        "Managed activation predecessor participant is unavailable.",
                    )
                })?;
            let latest = work
                .tx
                .latest_launch(predecessor.agent_id)
                .await?
                .filter(|launch| launch.identity == predecessor.identity)
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::IdentityConflict,
                        "Managed activation predecessor is no longer current.",
                    )
                })?;
            let session = work
                .tx
                .session(predecessor.agent_id)
                .await?
                .filter(|session| {
                    session.agent_id == predecessor.agent_id
                        && session.context_id == predecessor_context
                })
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::IdentityConflict,
                        "Managed activation predecessor is no longer current.",
                    )
                })?;
            debug_assert_eq!(latest.agent_id, predecessor.agent_id);
            debug_assert_eq!(session.agent_id, latest.agent_id);
            if predecessor_agent.host != request.target_context.host
                || predecessor_agent.host_session_id == request.target_context.host_session_id
                || !self.host.launch_root_departed(
                    self.project_id(),
                    predecessor_agent.host,
                    &predecessor_agent.host_session_id,
                    &predecessor.identity,
                )
            {
                return Err(Error::new(
                    ErrorCode::IdentityConflict,
                    "Managed activation predecessor lacks an exact distinct-root departure.",
                ));
            }
            if predecessor.retired_at.is_none() {
                launch::retire(&mut work, predecessor, "root_changed").await?;
            }
        }
        let value = self
            .activate_join(
                &mut work,
                &request.target_context,
                request.name.as_deref(),
                verified.state,
                verified.identity,
            )
            .await?;
        let result = decode_result(&value)?;
        work.tx
            .save_idempotency(&IdempotencyRecord {
                request_id: request.request_id,
                actor_key,
                operation: "managed_activation".into(),
                argument_hash: hash,
                result: value,
                committed_at: now,
            })
            .await?;
        tx.commit().await?;
        Ok(result)
    }
}

fn validate_cause(request: &ManagedActivationRequest) -> Result<()> {
    if let ManagedActivationCause::DistinctRoot {
        predecessor_context,
        predecessor_generation,
        transition_epoch,
    } = request.cause
        && (predecessor_context.is_nil()
            || predecessor_generation.is_nil()
            || transition_epoch == 0
            || predecessor_context == request.target_context.context_id)
    {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Distinct-root activation requires a fresh correlated predecessor and epoch.",
        ));
    }
    Ok(())
}

fn argument_hash(request: &ManagedActivationRequest) -> Result<String> {
    let encoded = serde_json::to_vec(request)
        .map_err(|_| Error::new(ErrorCode::IntegrityError, "Activation encoding failed."))?;
    Ok(Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn actor_key(request: &ManagedActivationRequest) -> Result<String> {
    serde_json::to_string(&(
        request.target_context.host,
        &request.target_context.host_session_id,
    ))
    .map_err(|_| {
        Error::new(
            ErrorCode::IntegrityError,
            "Activation actor encoding failed.",
        )
    })
}

fn decode_result(value: &serde_json::Value) -> Result<ManagedActivationResult> {
    let field = |name: &str| {
        value.get(name).cloned().ok_or_else(|| {
            Error::new(
                ErrorCode::IntegrityError,
                "Managed activation receipt is incomplete.",
            )
        })
    };
    Ok(ManagedActivationResult {
        agent_id: serde_json::from_value(field("agent_id")?).map_err(|_| invalid_receipt())?,
        context_id: serde_json::from_value(field("context_id")?).map_err(|_| invalid_receipt())?,
        generation: serde_json::from_value(field("generation")?).map_err(|_| invalid_receipt())?,
        join_boundary: serde_json::from_value(field("join_boundary")?)
            .map_err(|_| invalid_receipt())?,
    })
}

fn invalid_receipt() -> Error {
    Error::new(
        ErrorCode::IntegrityError,
        "Managed activation receipt contains invalid identities.",
    )
}
