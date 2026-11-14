//! One normalized mutation policy shared by single and grouped commits.
use super::{Application, Committed, Mutation, MutationRequest, Work, participation, reservations};
use crate::{
    Error, ErrorCode, Result,
    model::Context,
    ports::{HostGate, HostState, IdempotencyRecord, Transaction},
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(super) struct Prepared {
    request_id: Uuid,
    context: Context,
    command: Mutation,
    actor_key: String,
    argument_hash: String,
    host_state: Option<Result<HostState>>,
}
impl Prepared {
    pub(super) async fn capture_state(&mut self, host: &dyn HostGate) {
        // Native metadata verification must finish before acquiring the FULL
        // writer. Capture errors as values: the writer's immutable successful
        // receipt still wins on replay, subject to its original-owner recheck.
        self.host_state = Some(
            async {
                if matches!(self.command, Mutation::Join { .. })
                    && !host.allows_unmanaged_participation()
                    && host.launch_identity(&self.context).await?.is_none()
                {
                    return Err(super::activation::managed_required(&self.context));
                }
                host.verify(&self.context).await
            }
            .await,
        );
    }
}
impl Application {
    pub(super) fn prepare_request(&self, request: MutationRequest) -> Result<Prepared> {
        let MutationRequest {
            request_id,
            context,
            command,
        } = request;
        self.validate_context(&context)?;
        let command = command.normalize()?;
        let encoded = serde_json::to_vec(&command)
            .map_err(|_| Error::new(ErrorCode::IntegrityError, "Mutation normalization failed."))?;
        let argument_hash = Sha256::digest(&encoded)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let actor_key = serde_json::to_string(&(context.host, &context.host_session_id))
            .map_err(|_| Error::new(ErrorCode::IntegrityError, "Identity normalization failed."))?;
        Ok(Prepared {
            request_id,
            context,
            command,
            actor_key,
            argument_hash,
            host_state: None,
        })
    }
    // This private candidate is never returned to a transport until the outer
    // durability boundary commits. A nested savepoint release is not durable.
    pub(super) async fn apply_prepared(
        &self,
        tx: &mut dyn Transaction,
        request: Prepared,
    ) -> Result<Committed> {
        let Prepared {
            request_id,
            context,
            command,
            actor_key,
            argument_hash,
            host_state,
        } = request;
        let context = &context;
        if let Some(record) = tx.idempotency(request_id).await? {
            if record.actor_key != actor_key
                || record.operation != command.name()
                || record.argument_hash != argument_hash
            {
                return Err(Error::new(
                    ErrorCode::IdempotencyConflict,
                    "Request UUID already identifies a different successful mutation.",
                ));
            }
            if matches!(command, Mutation::Join { .. })
                && !self.host.allows_unmanaged_participation()
            {
                let identity = self
                    .host
                    .launch_identity(context)
                    .await?
                    .ok_or_else(|| super::activation::managed_required(context))?;
                if self.host.launch_owner(&identity) != crate::model::LaunchOwnerState::Current
                    || self.launch_root_changed(context, &identity)
                {
                    return Err(super::activation::managed_required(context));
                }
            }
            return Ok(Committed {
                result: record.result,
                event_head: None,
            });
        }
        if matches!(command, Mutation::Join { .. })
            && !self.host.allows_unmanaged_participation()
            && self.host.launch_identity(context).await?.is_none()
        {
            return Err(super::activation::managed_required(context));
        }
        // This gate consumes the coordinator's bounded live host evidence; it
        // must never invent an invitation or clear host approval/pause state.
        let state = match host_state {
            Some(state) => state?,
            None => {
                return Err(Error::new(
                    ErrorCode::IntegrityError,
                    "Mutation host observation was not captured before its writer.",
                ));
            }
        };
        if !matches!(command, Mutation::Join { .. }) {
            self.authorize_participation(tx, context, matches!(command, Mutation::Leave))
                .await?;
        }
        if state.phase == crate::model::Phase::Disconnected && !matches!(command, Mutation::Leave) {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Host conversation is disconnected.",
            ));
        }
        let now = self.clock.now();
        let mut work = Work {
            tx: &mut *tx,
            now,
            last_seq: None,
        };
        reservations::expire(&mut work).await?;
        let result = if let Mutation::Join { name } = &command {
            self.join_authorized(&mut work, context, name.as_deref(), state)
                .await?
        } else {
            let leaving = matches!(command, Mutation::Leave);
            let actor = work.actor(context, !leaving, !leaving).await?;
            if !leaving {
                participation::bind(&mut work, context, actor.id, state).await?;
            }
            if leaving {
                participation::leave(&mut work, actor).await?
            } else {
                super::actions::apply(&mut work, actor.id, &command).await?
            }
        };
        let event_head = work.last_seq;
        tx.save_idempotency(&IdempotencyRecord {
            request_id,
            actor_key,
            operation: command.name().into(),
            argument_hash,
            result: result.clone(),
            committed_at: now,
        })
        .await?;
        Ok(Committed { result, event_head })
    }
}
