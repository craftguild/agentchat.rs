//! Common subject routing and anonymous participation over the shared actions.

use super::{Application, Committed, Mutation, Work, actions, participation, reservations};
use crate::{
    Error, ErrorCode, Result,
    model::{Actor, ActorIdentity, OperationActor},
    ports::{IdempotencyRecord, Transaction},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

impl Application {
    /// Execute for an exactly resolved native context or the common anonymous user.
    /// Native selection retains every existing host/owner/generation gate. Users
    /// take the same action policy without a fake context or host verification.
    pub async fn execute_actor(
        &self,
        request_id: Uuid,
        actor: &OperationActor,
        command: Mutation,
    ) -> Result<Committed> {
        match actor {
            OperationActor::Native(context) => self.execute(request_id, context, command).await,
            OperationActor::User => self.execute_user(request_id, command).await,
        }
    }

    async fn execute_user(&self, request_id: Uuid, command: Mutation) -> Result<Committed> {
        let command = command.normalize()?;
        if matches!(command, Mutation::Join { name: Some(_) }) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Anonymous Join does not accept an agent name.",
            ));
        }
        let actor_key =
            serde_json::to_string(&("user", self.project_id())).map_err(|_| integrity())?;
        let argument_hash = Sha256::digest(serde_json::to_vec(&command).map_err(|_| integrity())?)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut tx = self.storage.begin().await?;
        if let Some(record) = tx.idempotency(request_id).await? {
            if record.actor_key != actor_key
                || record.operation != command.name()
                || record.argument_hash != argument_hash
            {
                return Err(Error::new(
                    ErrorCode::IdempotencyConflict,
                    "Request UUID identifies another successful mutation.",
                ));
            }
            return Ok(Committed {
                result: record.result,
                event_head: None,
            });
        }
        let mut work = Work {
            tx: tx.as_mut(),
            now: self.clock.now(),
            last_seq: None,
        };
        reservations::expire(&mut work).await?;
        let result = if matches!(command, Mutation::Join { .. }) {
            join(&mut work).await?
        } else {
            let actor = require_user(work.tx, !matches!(command, Mutation::Leave)).await?;
            if matches!(command, Mutation::Leave) {
                leave(&mut work, actor).await?
            } else {
                actions::apply(&mut work, actor.actor_id, &command).await?
            }
        };
        let event_head = work.last_seq;
        let now = work.now;
        tx.save_idempotency(&IdempotencyRecord {
            request_id,
            actor_key,
            operation: command.name().into(),
            argument_hash,
            result: result.clone(),
            committed_at: now,
        })
        .await?;
        tx.commit().await?;
        Ok(Committed { result, event_head })
    }
}

async fn require_user(tx: &mut dyn Transaction, joined: bool) -> Result<Actor> {
    tx.user()
        .await?
        .filter(|a| !joined || a.joined)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::NotJoined,
                "The project user must Join before this operation.",
            )
        })
}

async fn join(work: &mut Work<'_>) -> Result<Value> {
    let existing = work.tx.user().await?;
    if let Some(actor) = existing.as_ref().filter(|a| a.joined) {
        return Ok(join_result(actor));
    }
    let mut event = work.allocate("user.joined").await?;
    let actor = Actor {
        actor_id: existing.as_ref().map_or_else(Uuid::new_v4, |a| a.actor_id),
        identity: ActorIdentity::User,
        handle: "user".into(),
        joined: true,
        first_join_at: existing.as_ref().map_or(work.now, |a| a.first_join_at),
        join_seq: event.seq,
        leave_seq: existing.as_ref().and_then(|a| a.leave_seq),
    };
    work.tx.save_actor(&actor).await?;
    event.actor_id = Some(actor.actor_id);
    event.data = json!({"actor_id":actor.actor_id,"kind":"user","handle":"user"});
    work.record_and_notify(&event, &Default::default()).await?;
    let policy = work
        .tx
        .channel_policy()
        .await?
        .unwrap_or_else(crate::project::ProjectChannelPolicy::legacy);
    for name in policy.ordered_defaults() {
        participation::join_channel(work, actor.actor_id, name).await?;
    }
    Ok(join_result(&actor))
}

fn join_result(actor: &Actor) -> Value {
    json!({"actor_id":actor.actor_id,"kind":"user","handle":"user","participation":"joined","join_boundary":actor.join_seq})
}

async fn leave(work: &mut Work<'_>, mut actor: Actor) -> Result<Value> {
    if actor.joined {
        let mut event = work.allocate("user.left").await?;
        actor.joined = false;
        actor.leave_seq = Some(event.seq);
        work.tx.save_actor(&actor).await?;
        work.tx.remove_memberships(actor.actor_id).await?;
        work.tx.cancel_deliveries(actor.actor_id, work.now).await?;
        event.actor_id = Some(actor.actor_id);
        event.data = json!({"actor_id":actor.actor_id,"kind":"user","handle":"user"});
        work.record_and_notify(&event, &Default::default()).await?;
    }
    participation::explicit_cleanup(work, actor.actor_id).await?;
    Ok(
        json!({"actor_id":actor.actor_id,"kind":"user","participation":"left","leave_boundary":actor.leave_seq}),
    )
}

pub(super) fn nonreceiving() -> Error {
    let mut error = Error::new(
        ErrorCode::InvalidArgument,
        "Anonymous users have no receiving queue or delivery ACK operation.",
    );
    error.details = json!({"reason":"nonreceiving_actor"});
    error
}

fn integrity() -> Error {
    Error::new(
        ErrorCode::IntegrityError,
        "Actor mutation normalization failed.",
    )
}
