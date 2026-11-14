//! Exclusive group acquisition and terminal lease transitions under one writer.

use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::work::{Work, not_found};
use crate::{
    Error, ErrorCode, Result,
    glob::Glob,
    model::{LeaseState, Reservation, ReservationConflict},
};

pub(super) async fn expire(work: &mut Work<'_>) -> Result<usize> {
    let mut count = 0;
    let mut scan = Scan::default();
    while let Some(groups) = scan.next(work).await? {
        for mut group in groups {
            if group.expires_at <= work.now {
                group.state = LeaseState::Expired;
                work.tx.save_reservation(&group).await?;
                let data = result(work, &group).await?;
                work.informational("reservation.expired", None, None, data)
                    .await?;
                count += 1;
            }
        }
    }
    Ok(count)
}

pub(super) async fn acquire(
    work: &mut Work<'_>,
    owner: Uuid,
    patterns: &[String],
    ttl: u32,
    reason: Option<String>,
) -> Result<Value> {
    let requested = patterns.to_vec();
    let compiled = tokio::task::spawn_blocking(move || {
        requested
            .iter()
            .map(|p| Glob::compile(p))
            .collect::<Result<Vec<_>>>()
            .map(Arc::new)
    })
    .await
    .map_err(|_| worker_error())??;
    let mut scan = Scan::default();
    while let Some(retained) = scan.next(work).await? {
        let requested = compiled.clone();
        // The immediate transaction remains held while this worker decides exact
        // overlap. Releasing it before insertion would admit two racing owners.
        let collisions = tokio::task::spawn_blocking(move || conflicts(owner, requested, retained))
            .await
            .map_err(|_| worker_error())??;
        if let Some((group, requested_pattern, existing_pattern, witness_path)) = collisions {
            let mut error = Error::new(
                ErrorCode::ReservationConflict,
                "The requested paths overlap another actor's reservation.",
            );
            let agent = work.tx.actor(group.owner_id).await?.ok_or_else(|| {
                Error::new(ErrorCode::IntegrityError, "Reservation owner is missing.")
            })?;
            let detail = ReservationConflict {
                reservation_id: group.id,
                owner: (&agent).into(),
                requested_pattern,
                existing_pattern,
                expires_at: group.expires_at,
                witness_path,
            };
            error.details = json!({"conflicts": [detail]});
            return Err(error);
        }
    }
    let mut event = work.allocate("reservation.acquired").await?;
    let group = Reservation {
        id: Uuid::new_v4(),
        owner_id: owner,
        generation: event.seq.get(),
        state: LeaseState::Active,
        ttl_seconds: ttl,
        acquired_at: work.now,
        expires_at: work.now.after_seconds(i64::from(ttl))?,
        released_at: None,
        reason,
        patterns: patterns.to_vec(),
    };
    work.tx.save_reservation(&group).await?;
    event.actor_id = Some(owner);
    event.data = result(work, &group).await?;
    work.record_and_notify(&event, &Default::default()).await?;
    result(work, &group).await
}

type Collision = (Reservation, String, String, String);

fn conflicts(
    owner: Uuid,
    compiled: Arc<Vec<Glob>>,
    retained: Vec<Reservation>,
) -> Result<Option<Collision>> {
    for group in retained.into_iter().filter(|r| r.owner_id != owner) {
        for existing in &group.patterns {
            let other = Glob::compile(existing)?;
            for pattern in compiled.iter() {
                if let Some(witness) = pattern.overlap(&other)? {
                    // One exact witness proves that the complete acquisition
                    // must fail. Return bounded details without scanning more.
                    return Ok(Some((
                        group.clone(),
                        pattern.pattern().into(),
                        existing.clone(),
                        witness,
                    )));
                }
            }
        }
    }
    Ok(None)
}

fn worker_error() -> Error {
    Error::new(ErrorCode::IntegrityError, "Pattern worker failed.")
}

/// Immutable keyset ordering survives expiry/release of earlier scan rows.
#[derive(Default)]
pub(super) struct Scan {
    after: Option<Uuid>,
}

impl Scan {
    pub(super) async fn next(&mut self, work: &mut Work<'_>) -> Result<Option<Vec<Reservation>>> {
        let groups = work.tx.active_reservations(self.after).await?;
        if groups.is_empty() {
            return Ok(None);
        }
        if groups.len() > 200
            || groups.windows(2).any(|p| p[0].id >= p[1].id)
            || self.after.is_some_and(|a| groups[0].id <= a)
        {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "Reservation scan failed to advance in bounded order.",
            ));
        }
        self.after = groups.last().map(|g| g.id);
        Ok(Some(groups))
    }
}

async fn owned(work: &mut Work<'_>, owner: Uuid, id: Uuid, generation: i64) -> Result<Reservation> {
    let group = work
        .tx
        .reservation(id)
        .await?
        .ok_or_else(|| not_found("Reservation does not exist."))?;
    if group.owner_id != owner || group.generation != generation {
        return Err(Error::new(
            ErrorCode::StaleLease,
            "Reservation owner or generation does not match.",
        ));
    }
    Ok(group)
}

pub(super) async fn renew(
    work: &mut Work<'_>,
    owner: Uuid,
    id: Uuid,
    generation: i64,
) -> Result<Value> {
    let mut group = owned(work, owner, id, generation).await?;
    if group.state != LeaseState::Active || group.expires_at <= work.now {
        return Err(Error::new(
            ErrorCode::StaleLease,
            "A terminal or expired lease cannot be renewed.",
        ));
    }
    group.expires_at = group
        .expires_at
        .max(work.now.after_seconds(i64::from(group.ttl_seconds))?);
    work.tx.save_reservation(&group).await?;
    result(work, &group).await
}

pub(super) async fn release(
    work: &mut Work<'_>,
    owner: Uuid,
    id: Uuid,
    generation: i64,
) -> Result<Value> {
    let mut group = owned(work, owner, id, generation).await?;
    match group.state {
        LeaseState::Released => return result(work, &group).await,
        LeaseState::Expired => {
            return Err(Error::new(
                ErrorCode::StaleLease,
                "An expired lease is no longer owned.",
            ));
        }
        LeaseState::Active => {}
    }
    release_group(work, &mut group).await?;
    result(work, &group).await
}

pub(super) async fn release_group(work: &mut Work<'_>, group: &mut Reservation) -> Result<()> {
    group.state = LeaseState::Released;
    group.released_at = Some(work.now);
    work.tx.save_reservation(group).await?;
    let data = result(work, group).await?;
    work.informational("reservation.released", Some(group.owner_id), None, data)
        .await?;
    Ok(())
}

pub(super) async fn result(work: &mut Work<'_>, group: &Reservation) -> Result<Value> {
    let owner =
        work.tx.actor(group.owner_id).await?.ok_or_else(|| {
            Error::new(ErrorCode::IntegrityError, "Reservation owner is missing.")
        })?;
    serde_json::to_value(crate::read::ReservationView::with_owner(
        group.clone(),
        (&owner).into(),
    ))
    .map_err(|_| {
        Error::new(
            ErrorCode::IntegrityError,
            "Reservation result encoding failed.",
        )
    })
}
