//! O(1) descriptor-to-owner lookup. An index is never native or admission proof.
use super::{
    NativeLifecycle, metadata,
    namespace::OwnerNamespace,
    observer::{NativeProfile, RegisteredOwner},
};
use agentchat_core::{Error, ErrorCode, Result, model::Context};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u32,
    native: RegisteredOwner,
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The exact current native context owner is unavailable.",
    )
}
fn directory(home: &Path, create: bool) -> Result<PathBuf> {
    let mut path = home.to_owned();
    metadata::private_directory(&path)?;
    for part in ["hosts", "claude", "contexts"] {
        path.push(part);
        if create {
            super::namespace::private(&path)?;
        } else {
            metadata::private_directory(&path)?;
        }
    }
    Ok(path)
}
/// Publish only immutable owner lookup metadata under the caller's exact-current
/// owner-cursor lock. A partial publication grants nothing and the same callback
/// may repair it. Never call this from tool arguments or generic discovery.
pub fn publish(home: &Path, native: &RegisteredOwner) -> Result<()> {
    NativeLifecycle::Project(native.registration.clone()).validate()?;
    if !native.owner.is_continuing() {
        return Err(required());
    }
    super::signal::prepare(home, &native.registration.context.working_directory)?;
    let path =
        directory(home, true)?.join(format!("{}.json", native.registration.context.context_id));
    let wanted = Index {
        version: 1,
        native: native.clone(),
    };
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            if metadata::read::<Index>(&path)? != wanted {
                return Err(required());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            metadata::Publisher::new(home).create(&path, &wanted)?
        }
        Err(_) => return Err(required()),
    }
    Ok(())
}
/// Remove only this exact owned lookup under the matching lifecycle lock.
/// Registry descriptors/history and changed/foreign records are preserved.
pub fn remove(home: &Path, native: &RegisteredOwner) -> Result<bool> {
    if !home
        .join("hosts/claude/contexts")
        .try_exists()
        .map_err(|_| required())?
    {
        return Ok(false);
    }
    let path =
        directory(home, false)?.join(format!("{}.json", native.registration.context.context_id));
    metadata::remove_if(
        &path,
        &Index {
            version: 1,
            native: native.clone(),
        },
    )
}
pub(super) struct Route {
    pub namespace: OwnerNamespace,
    pub native: RegisteredOwner,
    pub profile: NativeProfile,
    committed: super::namespace::CommittedLifecycle,
    home: PathBuf,
}
impl Route {
    pub fn reservation(&self) -> super::namespace::Reservation {
        self.committed.reservation.clone()
    }
    pub async fn check(&self) -> Result<()> {
        if read_index(
            self.home.clone(),
            self.native.registration.context.context_id,
        )
        .await?
            != (Index {
                version: 1,
                native: self.native.clone(),
            })
            || self.namespace.read_current().await?.as_ref() != Some(&self.committed)
            || self.committed.reservation.transport_nonce
                != Some(
                    self.namespace
                        .transport()
                        .await?
                        .ok_or_else(required)?
                        .nonce,
                )
            || !self.native.owner.is_continuing()
        {
            return Err(required());
        }
        Ok(())
    }
}
pub(super) async fn resolve(home: &Path, context: &Context) -> Result<Route> {
    let index = read_index(home.to_owned(), context.context_id).await?;
    if index.version != 1 || index.native.registration.context != *context {
        return Err(required());
    }
    NativeLifecycle::Project(index.native.registration.clone()).validate()?;
    let namespace = OwnerNamespace::existing(home, index.native.owner.clone()).await?;
    let committed = namespace.read_current().await?.ok_or_else(required)?;
    if committed.lifecycle != NativeLifecycle::Project(index.native.registration.clone()) {
        return Err(required());
    }
    let path = namespace.directory().join("native-profile.json");
    let profile: NativeProfile = tokio::task::spawn_blocking(move || metadata::read(&path))
        .await
        .map_err(|_| required())??;
    if !super::module::candidate_version(&profile.version)
        || context.host_version != profile.version
        || context.os_profile != format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        || profile.executable != index.native.owner.executable()
    {
        return Err(required());
    }
    let route = Route {
        namespace,
        native: index.native,
        profile,
        committed,
        home: home.to_owned(),
    };
    route.check().await?;
    Ok(route)
}
async fn read_index(home: PathBuf, context: uuid::Uuid) -> Result<Index> {
    tokio::task::spawn_blocking(move || {
        metadata::read(&directory(&home, false)?.join(format!("{context}.json")))
    })
    .await
    .map_err(|_| required())?
}
