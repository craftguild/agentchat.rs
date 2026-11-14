//! Integrity proof scoped to one Immediate writer and all its savepoints.
//!
//! Independent writers cannot change this snapshot. Only the tracked typed
//! allocator/publisher can change the Dispatch guard's inputs. Root commits
//! still perform the complete guard, independently of this savepoint proof.
use super::dispatches;
use crate::error::integrity;
use agentchat_core::Result;
use sea_orm::ConnectionTrait;
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, MutexGuard},
};
use uuid::Uuid;

const MAX_TRACKED_HEADERS: usize = 256;

#[derive(Default)]
struct State {
    validated: bool,
    fallback: bool,
    touched: BTreeSet<Uuid>,
}

#[derive(Clone, Default)]
pub(super) struct Proof(Arc<Mutex<State>>);

impl Proof {
    fn state(&self) -> Result<MutexGuard<'_, State>> {
        self.0
            .lock()
            .map_err(|_| integrity("Transaction integrity proof is poisoned."))
    }

    /// Register before any potentially partial write. Never forget IDs after
    /// validation or rollback: an ancestor rollback can restore a parent draft.
    pub fn touch(&self, id: Uuid) -> Result<()> {
        let mut state = self.state()?;
        if !state.fallback && !state.touched.contains(&id) {
            if state.touched.len() == MAX_TRACKED_HEADERS {
                state.fallback = true;
                state.touched.clear();
            } else {
                state.touched.insert(id);
            }
        }
        Ok(())
    }

    pub async fn check<C: ConnectionTrait>(&self, db: &C, root: bool) -> Result<()> {
        let touched = {
            let state = self.state()?;
            if root || !state.validated || state.fallback {
                None
            } else {
                Some(state.touched.iter().copied().collect::<Vec<_>>())
            }
        };
        if let Some(ids) = touched {
            dispatches::guard_changed(db, &ids).await
        } else {
            dispatches::guard(db).await?;
            // An unsuccessful guard never establishes a reusable proof.
            self.state()?.validated = true;
            Ok(())
        }
    }
}
