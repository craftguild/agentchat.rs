//! One pending random allocation is durable before asynchronous publication.
use super::{OwnerNamespace, Reservation, state::Header, unavailable};
use crate::claude::{NativeLifecycle, NativeRegistration};
use agentchat_core::{Error, ErrorCode, Result, model::Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::unix::ffi::OsStrExt;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Allocation {
    Unassigned,
    Assigned {
        ticket: Uuid,
        context_id: Uuid,
        incarnation: Uuid,
        source_sha256: [u8; 32],
    },
}
pub(super) fn validate(h: &Header) -> Result<()> {
    if h.version == 1 {
        return if h.allocation.is_none() {
            Ok(())
        } else {
            Err(unavailable())
        };
    }
    match (&h.pending, &h.allocation) {
        (None, None) | (Some(_), Some(Allocation::Unassigned)) => Ok(()),
        (
            Some(p),
            Some(Allocation::Assigned {
                ticket,
                context_id,
                incarnation,
                ..
            }),
        ) if *ticket == p.ticket
            && *context_id != *incarnation
            && context_id.get_version() == Some(uuid::Version::Random)
            && incarnation.get_version() == Some(uuid::Version::Random) =>
        {
            Ok(())
        }
        _ => Err(unavailable()),
    }
}
fn source(context: &Context) -> Result<[u8; 32]> {
    let bytes = serde_json::to_vec(&(
        context.project_id,
        context.host,
        &context.host_session_id,
        &context.host_version,
        &context.os_profile,
        context.working_directory.as_os_str().as_bytes(),
    ))
    .map_err(|_| unavailable())?;
    Ok(Sha256::new()
        .chain_update(b"agentchat/native-allocation-source/v1\0")
        .chain_update(bytes)
        .finalize()
        .into())
}
fn conflict() -> Error {
    Error::new(
        ErrorCode::IdempotencyConflict,
        "Native allocation intent has different identity or routing facts.",
    )
}
fn project(h: &Header, intent: &Reservation, context: Context) -> NativeLifecycle {
    NativeLifecycle::Project(NativeRegistration {
        owner_root: h.namespace_id,
        observer: h.observer.clone(),
        generation: intent.generation,
        root: intent.root,
        context,
    })
}
pub(super) fn check_commit(
    h: &Header,
    intent: &Reservation,
    lifecycle: &NativeLifecycle,
) -> Result<()> {
    if let Some(Allocation::Assigned {
        ticket,
        context_id,
        incarnation,
        source_sha256,
    }) = &h.allocation
        && *ticket == intent.ticket
        && let NativeLifecycle::Project(r) = lifecycle
        && (r.context.context_id != *context_id
            || r.context.incarnation != *incarnation
            || source(&r.context)? != *source_sha256)
    {
        return Err(conflict());
    }
    Ok(())
}
impl OwnerNamespace {
    /// Persist or recover one pending intent's random descriptor references.
    /// Only call after exact native intent validation and project preparation;
    /// generic tool arguments must not choose these facts. No registry/IPC/native
    /// work runs under the owner lock. The atomic original header write finishes
    /// before references return, so a killed publisher cannot allocate a second
    /// identity on retry. Reading the allocation grants no host/root authority.
    pub async fn allocate_context(
        &self,
        intent: Reservation,
        mut proposed: Context,
    ) -> Result<Context> {
        self.update(move |h| {
            project(h, &intent, proposed.clone()).validate()?;
            if proposed.working_directory != intent.working_directory {
                return Err(unavailable());
            }
            if !h.retired
                && h.pending.is_none()
                && let Some(c) = &h.current
                && c.reservation == intent
                && let NativeLifecycle::Project(r) = &c.lifecycle
            {
                return if source(&proposed)? == source(&r.context)? {
                    Ok(r.context.clone())
                } else {
                    Err(conflict())
                };
            }
            if h.pending.as_ref() != Some(&intent) {
                return Err(unavailable());
            }
            let fingerprint = source(&proposed)?;
            match &h.allocation {
                Some(Allocation::Assigned {
                    ticket,
                    context_id,
                    incarnation,
                    source_sha256,
                }) => {
                    if *ticket != intent.ticket || *source_sha256 != fingerprint {
                        return Err(conflict());
                    }
                    proposed.context_id = *context_id;
                    proposed.incarnation = *incarnation;
                }
                Some(Allocation::Unassigned) | None => {
                    // None is admitted only for a validated format-one pending
                    // header. Format two requires an explicit allocation state.
                    h.version = 2;
                    h.allocation = Some(Allocation::Assigned {
                        ticket: intent.ticket,
                        context_id: proposed.context_id,
                        incarnation: proposed.incarnation,
                        source_sha256: fingerprint,
                    });
                }
            }
            h.validate_commit(&intent, &project(h, &intent, proposed.clone()))?;
            Ok(proposed)
        })
        .await
    }
}
