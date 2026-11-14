//! Exact-incarnation restoration, host liveness and scheduled lease control.

use serde::{Deserialize, Serialize};

use super::{Application, Work, participation, reservations};
use crate::{
    Error, ErrorCode, Result,
    model::{Agent, Context, Phase},
    sequence::Sequence,
};

/// Committed lease housekeeping; callers may signal observers after return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maintenance {
    /// Launches atomically retired after positive original-owner loss.
    pub retired_launches: usize,
    /// Unique newly committed unconfirmed-delivery notifications in this bounded tick.
    pub delivery_confirmation_timeouts: usize,
    /// Newly materialized expiry transitions.
    pub expired: usize,
    /// Eligible connected groups whose deadline was extended.
    pub renewed: usize,
    /// Last new business event; renewal alone emits no event.
    pub event_head: Option<Sequence>,
}

impl Application {
    /// A runtime restart cannot reuse another process's persisted liveness.
    /// Call only after obtaining exclusive runtime ownership, before renewal.
    pub async fn invalidate_connections(&self) -> Result<()> {
        let mut tx = self.storage.begin().await?;
        tx.disconnect_sessions().await?;
        tx.commit().await
    }
    /// Restore only an already retained conversation, preserving joined/left.
    /// New/forked session IDs return none and never acquire an agent identity.
    pub async fn attach(&self, context: &Context) -> Result<Option<Agent>> {
        self.validate_context(context)?;
        let state = self.host.verify(context).await?;
        if state.phase == Phase::Disconnected {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Cannot attach an offline host conversation.",
            ));
        }
        let mut tx = self.storage.begin().await?;
        let Some(agent) = tx
            .conversation(context.host, &context.host_session_id)
            .await?
        else {
            return Ok(None);
        };
        if !agent.joined {
            return Err(Error::new(
                ErrorCode::NotJoined,
                "This conversation has left; restoration never invites it.",
            ));
        }
        self.authorize_participation(tx.as_mut(), context, false)
            .await?;
        let mut work = Work {
            tx: tx.as_mut(),
            now: self.clock.now(),
            last_seq: None,
        };
        participation::bind(&mut work, context, agent.id, state).await?;
        tx.commit().await?;
        Ok(Some(agent))
    }

    /// Refresh actual host evidence for the current live incarnation only.
    /// The adapter owns pause/approval semantics; a helper timer is not evidence.
    pub async fn heartbeat(&self, context: &Context) -> Result<()> {
        self.validate_context(context)?;
        let state = self.host.verify(context).await?;
        if state.phase == Phase::Disconnected {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Host conversation is disconnected.",
            ));
        }
        let mut tx = self.storage.begin().await?;
        self.authorize_participation(tx.as_mut(), context, false)
            .await?;
        let mut work = Work {
            tx: tx.as_mut(),
            now: self.clock.now(),
            last_seq: None,
        };
        let agent = work.actor(context, false, true).await?;
        participation::bind(&mut work, context, agent.id, state).await?;
        tx.commit().await
    }

    /// Record an authoritative disconnect without leaving or releasing leases.
    /// A late callback for an older incarnation cannot disconnect its successor.
    pub async fn detach(&self, context: &Context) -> Result<()> {
        self.validate_context(context)?;
        let mut tx = self.storage.begin().await?;
        if tx
            .conversation(context.host, &context.host_session_id)
            .await?
            .is_none()
        {
            return Ok(());
        }
        let mut work = Work {
            tx: tx.as_mut(),
            now: self.clock.now(),
            last_seq: None,
        };
        let agent = work.actor(context, false, false).await?;
        let mut session = work.tx.session(agent.id).await?.ok_or_else(|| {
            Error::new(
                ErrorCode::SessionRequired,
                "Current host connection is missing.",
            )
        })?;
        session.phase = Phase::Disconnected;
        work.tx.save_session(&session).await?;
        tx.commit().await
    }

    /// Materialize deadlines and renew eligible live owners without model traffic.
    /// Run at startup and at the minimum connected lease renewal interval.
    pub async fn maintain(&self) -> Result<Maintenance> {
        let mut tx = self.storage.begin().await?;
        let mut work = Work {
            tx: tx.as_mut(),
            now: self.clock.now(),
            last_seq: None,
        };
        let retired_launches = self.maintain_launches(&mut work).await?;
        let expired = reservations::expire(&mut work).await?;
        let delivery_confirmation_timeouts = super::confirmations::expire(&mut work).await?;
        let mut renewed = 0;
        let mut scan = reservations::Scan::default();
        while let Some(groups) = scan.next(&mut work).await? {
            for mut group in groups {
                let interval = i64::from((group.ttl_seconds / 3).min(60));
                let due = group
                    .expires_at
                    .after_seconds(-i64::from(group.ttl_seconds))?
                    .after_seconds(interval)?;
                if due > work.now {
                    continue;
                }
                let joined = work
                    .tx
                    .agent(group.owner_id)
                    .await?
                    .is_some_and(|a| a.joined);
                let live = work
                    .tx
                    .session(group.owner_id)
                    .await?
                    .is_some_and(|s| s.is_connected(work.now));
                let authority = match work.tx.latest_launch(group.owner_id).await? {
                    Some(launch) => {
                        launch.retired_at.is_none()
                            && self.host.launch_owner(&launch.identity)
                                == crate::model::LaunchOwnerState::Current
                    }
                    None => self.host.allows_unmanaged_participation(),
                };
                if joined && live && authority {
                    group.expires_at = work.now.after_seconds(i64::from(group.ttl_seconds))?;
                    work.tx.save_reservation(&group).await?;
                    renewed += 1;
                }
            }
        }
        let event_head = work.last_seq;
        tx.commit().await?;
        Ok(Maintenance {
            retired_launches,
            delivery_confirmation_timeouts,
            expired,
            renewed,
            event_head,
        })
    }
}
