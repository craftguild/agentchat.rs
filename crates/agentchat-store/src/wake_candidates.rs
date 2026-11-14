//! Fresh read-only pruning never grants a host or durable presentation claim.

mod deferred;

use crate::{
    decode,
    entities::{actor as agent, delivery, dispatch, wake_batch},
    error::{database, integrity},
};
use agentchat_core::{
    Error, ErrorCode, Result, delivery::MAX_WAKE_SCAN_CONTEXTS, model::Context, time::Timestamp,
};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, TransactionTrait,
    sea_query::{Condition, Expr, ExprTrait, Query},
};
use uuid::Uuid;

type Candidate<'a> = (Uuid, &'a Context);

pub(crate) async fn select(
    db: &DatabaseConnection,
    contexts: &[Context],
    now: Timestamp,
) -> Result<Vec<Uuid>> {
    if contexts.len() > MAX_WAKE_SCAN_CONTEXTS {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Wake candidate scan exceeds 32 contexts.",
        ));
    }
    if contexts.is_empty() {
        return Ok(Vec::new());
    }
    // The first selection anchors one deferred read transaction on the separate
    // read-only connection. Batch state and assigned rows must not come from
    // different commits. Explicit rollback releases this snapshot on all results.
    let tx = db.begin().await.map_err(database)?;
    let result = scan(&tx, contexts, now).await;
    tx.rollback().await.map_err(database)?;
    result
}

async fn scan<C: ConnectionTrait>(
    db: &C,
    contexts: &[Context],
    now: Timestamp,
) -> Result<Vec<Uuid>> {
    let candidates = candidates(db, contexts).await?;
    let blocked = deferred::blocked(db, &candidates, now).await?;
    Ok(candidates
        .into_iter()
        .filter(|(agent, context)| !blocked.contains(&(*agent, context.incarnation)))
        .map(|(_, context)| context.context_id)
        .collect())
}

async fn candidates<'a, C: ConnectionTrait>(
    db: &C,
    contexts: &'a [Context],
) -> Result<Vec<Candidate<'a>>> {
    let mut conversations = Condition::any();
    for context in contexts {
        conversations = conversations.add(
            Condition::all()
                .add(Expr::col((agent::Entity, agent::Column::HostKind)).eq(context.host.as_str()))
                .add(
                    Expr::col((agent::Entity, agent::Column::HostSessionId))
                        .eq(&context.host_session_id),
                ),
        );
    }
    // Start with the conservative superset, including other incarnations and
    // fully settled sets. The separate refinement removes only proven deferrals;
    // phase/liveness/participation decisions stay in fresh preparation.
    let batches = Query::select()
        .column(wake_batch::Column::Id)
        .from(wake_batch::Entity)
        .and_where(
            Expr::col((wake_batch::Entity, wake_batch::Column::RecipientId))
                .equals((agent::Entity, agent::Column::Id)),
        )
        .and_where(Expr::col(wake_batch::Column::State).is_in(["pending", "inflight", "stalled"]))
        .to_owned();
    let deliveries = Query::select()
        .column((delivery::Entity, delivery::Column::Id))
        .from(delivery::Entity)
        .inner_join(
            dispatch::Entity,
            Expr::col((dispatch::Entity, dispatch::Column::Id))
                .equals((delivery::Entity, delivery::Column::DispatchId)),
        )
        .and_where(
            Expr::col((delivery::Entity, delivery::Column::RecipientId))
                .equals((agent::Entity, agent::Column::Id)),
        )
        .and_where(
            Expr::col((delivery::Entity, delivery::Column::State)).is_in(["pending", "processing"]),
        )
        .and_where(Expr::col((delivery::Entity, delivery::Column::WakeBatchId)).is_null())
        .and_where(Expr::col((delivery::Entity, delivery::Column::WakeEligible)).eq(true))
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Sealed)).eq(true))
        .to_owned();
    let query = Query::select()
        .columns([
            agent::Column::Id,
            agent::Column::HostKind,
            agent::Column::HostSessionId,
        ])
        .from(agent::Entity)
        .cond_where(conversations)
        .cond_where(
            Condition::any()
                .add(Expr::exists(batches))
                .add(Expr::exists(deliveries)),
        )
        .limit(MAX_WAKE_SCAN_CONTEXTS as u64 + 1)
        .to_owned();
    let rows = db.query_all(&query).await.map_err(database)?;
    if rows.len() > MAX_WAKE_SCAN_CONTEXTS {
        return Err(integrity(
            "Wake candidate identities exceed the bounded input.",
        ));
    }
    let mut selected = Vec::new();
    for row in rows {
        let agent = decode::uuid(&row.try_get::<String>("", "id").map_err(database)?)?;
        let host: String = row.try_get("", "host_kind").map_err(database)?;
        let session: String = row.try_get("", "host_session_id").map_err(database)?;
        selected.extend(
            contexts
                .iter()
                .filter(|c| c.host.as_str() == host && c.host_session_id == session)
                .map(|c| (agent, c)),
        );
    }
    Ok(selected)
}
