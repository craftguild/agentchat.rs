//! Opaque exact-host routing descriptors; issuing one never joins or wakes.

use crate::{
    filesystem as fs,
    project::{ProjectManager, blocking},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Context, HostKind},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
};
use uuid::Uuid;

mod selector;

/// Verified lifecycle facts supplied by a host adapter, never guessed from cwd.
#[derive(Debug, Clone)]
pub struct ContextSeed {
    /// Exact independent root host kind.
    pub host: HostKind,
    /// Exact root conversation/session identifier from host lifecycle input.
    pub host_session_id: String,
    /// Actual host version.
    pub host_version: String,
    /// Exact host/OS profile.
    pub os_profile: String,
    /// Working directory is only for project routing, not conversation identity.
    pub working_directory: PathBuf,
}
/// Persistent opaque descriptor lookup for initialized namespaces only.
#[derive(Clone)]
pub struct ContextRegistry {
    manager: ProjectManager,
}
impl ContextRegistry {
    /// Validate complete immutable stage identity without current-route discovery.
    /// Recovery retains the original references even if the project/cwd changed;
    /// fresh application operations still require `resolve` and host proof.
    pub fn validate_stage(bytes: &[u8], target: &std::ffi::OsStr) -> Result<()> {
        if bytes.len() > 128 * 1024 {
            return Err(binding());
        }
        let name = target.to_str().ok_or_else(binding)?;
        let id = name
            .strip_suffix(".json")
            .and_then(|n| Uuid::parse_str(n).ok())
            .ok_or_else(binding)?;
        if id.is_nil() || name != format!("{id}.json") {
            return Err(binding());
        }
        let stored: Stored = serde_json::from_slice(bytes).map_err(|_| binding())?;
        let context = stored.context(id)?;
        if context.incarnation.is_nil() || context.project_id.is_nil() {
            return Err(binding());
        }
        Ok(())
    }
    /// Compose the shared project resolver without creating metadata.
    pub fn new(manager: ProjectManager) -> Self {
        Self { manager }
    }
    /// Issue fresh descriptor/incarnation IDs after exact lifecycle identification.
    /// This writes routing metadata only: no database, runtime or participant.
    pub async fn issue(&self, seed: ContextSeed) -> Result<Context> {
        let context = self.prepare(seed).await?;
        self.persist(context, false).await
    }
    /// Prepare fresh UUIDv4 references after exact lifecycle/project discovery.
    /// This creates no descriptor or other files. A trusted native caller may
    /// persist the chosen references in its original pending intent before
    /// publishing; preparation alone grants no participation or host authority.
    pub async fn prepare(&self, seed: ContextSeed) -> Result<Context> {
        if seed.host_session_id.is_empty()
            || seed.host_session_id.len() > 1024
            || seed.host_version.is_empty()
            || seed.host_version.len() > 128
            || seed.os_profile.is_empty()
            || seed.os_profile.len() > 128
        {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Exact bounded host lifecycle facts are required.",
            ));
        }
        let working_directory = seed.working_directory.clone();
        let working_directory =
            blocking(move || working_directory.canonicalize().map_err(fs::io)).await?;
        let project = self
            .manager
            .discover(&working_directory)
            .await?
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::NotInitialized,
                    "No initialized project owns this lifecycle directory.",
                )
            })?;
        let context = Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            project_id: project.identity().id,
            host: seed.host,
            host_session_id: seed.host_session_id,
            host_version: seed.host_version,
            os_profile: seed.os_profile,
            working_directory,
        };
        Ok(context)
    }
    /// Publish the immutable descriptor selected by a trusted lifecycle intent.
    /// The native caller must choose/reuse references under its original owner
    /// fence, rather than accept context values from generic tool arguments.
    /// Exact retries resynchronize the same full record, including after a
    /// competing or ambiguously acknowledged publication. Changed records are
    /// conflicts; new publication still requires the captured current route.
    pub async fn publish(&self, context: Context) -> Result<Context> {
        if context.context_id.is_nil()
            || context.incarnation.is_nil()
            || context.project_id.is_nil()
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Complete descriptor references are required.",
            ));
        }
        Stored::from(&context).context(context.context_id)?;
        let directory = context.working_directory.clone();
        let canonical = blocking(move || directory.canonicalize().map_err(fs::io)).await?;
        if canonical != context.working_directory
            || self
                .manager
                .discover(&canonical)
                .await?
                .is_none_or(|p| p.identity().id != context.project_id)
        {
            return Err(binding());
        }
        self.persist(context, true).await
    }
    async fn persist(&self, context: Context, replay: bool) -> Result<Context> {
        let _activity = self.manager.activity(context.project_id).await?;
        if self
            .manager
            .discover(&context.working_directory)
            .await?
            .is_none_or(|p| p.identity().id != context.project_id)
        {
            return Err(binding());
        }
        let home = self.manager.home().to_owned();
        blocking(move || {
            fs::private(&home, true)?;
            let directory = home.join("contexts");
            fs::directory(&directory)?;
            let path = directory.join(format!("{}.json", context.context_id));
            if replay && present(&path)? {
                retain(&path, &context)?;
            } else if let Err(error) =
                fs::write_new(&home, &path, &Stored::from(&context), 128 * 1024)
            {
                // A competing exact creator may have won, or a publication may
                // be visible after a failed directory sync. Never replace it.
                if !replay || !present(&path)? {
                    return Err(error);
                }
                retain(&path, &context)?;
            }
            Ok(context)
        })
        .await
    }
    pub(crate) fn removal_records(manager: &ProjectManager, project: Uuid) -> Result<Vec<Uuid>> {
        let directory = manager.home().join("contexts");
        match std::fs::symlink_metadata(&directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(fs::io(e)),
            Ok(_) => fs::private(&directory, true)?,
        }
        let mut ids = Vec::new();
        for (index, entry) in std::fs::read_dir(&directory).map_err(fs::io)?.enumerate() {
            if index >= 65_536 {
                return Err(fs::error("Descriptor inventory exceeds its bound."));
            }
            let entry = entry.map_err(fs::io)?;
            let name = entry.file_name();
            // Publication staging is not a committed descriptor. It remains
            // outside this exact selection; its existing reaper owns cleanup.
            let Some(id) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            if name != std::ffi::OsStr::new(&format!("{id}.json")) {
                return Err(binding());
            }
            let stored: Stored = fs::read(&entry.path(), 128 * 1024)?;
            let context = stored.context(id)?;
            if context.project_id == project {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }
    /// Resolve the exact descriptor, verifying its namespace still owns the cwd.
    /// This is routing validation, not proof of host liveness/admission.
    pub async fn resolve(&self, id: Uuid, expected: Option<Uuid>) -> Result<Context> {
        let context = self.descriptor(id).await?;
        if expected.is_some_and(|project| project != context.project_id) {
            return Err(binding());
        }
        let project = self
            .manager
            .discover(&context.working_directory)
            .await?
            .ok_or_else(binding)?;
        if project.identity().id != context.project_id {
            return Err(binding());
        }
        Ok(context)
    }
    /// Read immutable exact identity facts without claiming current routing.
    /// Only successful/pending initialization retry lookup may precede route
    /// validation; fresh operations must use `resolve` before making changes.
    /// Retain these records as identity/retry history even after owner metadata
    /// is collected; absence of a live owner is not permission to prune them.
    pub async fn descriptor(&self, id: Uuid) -> Result<Context> {
        let home = self.manager.home().to_owned();
        let stored: Stored = blocking(move || {
            let path = home.join("contexts").join(format!("{id}.json"));
            if let Err(error) = std::fs::symlink_metadata(&path) {
                return Err(if error.kind() == std::io::ErrorKind::NotFound {
                    Error::new(
                        ErrorCode::SessionRequired,
                        "Exact host descriptor is unavailable.",
                    )
                } else {
                    fs::io(error)
                });
            }
            fs::private(&home, true)?;
            fs::private(&home.join("contexts"), true)?;
            fs::read(&path, 128 * 1024)
        })
        .await?;
        stored.context(id)
    }
}
fn present(path: &std::path::Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(fs::io(e)),
    }
}
fn retain(path: &std::path::Path, expected: &Context) -> Result<()> {
    let stored: Stored = fs::read(path, 128 * 1024)?;
    if stored.context(expected.context_id)? != *expected {
        return Err(Error::new(
            ErrorCode::IdempotencyConflict,
            "Immutable descriptor UUID has different identity or routing facts.",
        ));
    }
    // Synchronize an already-visible winning publication before acknowledging
    // recovery. Immutable records grant no live-host or participation authority.
    std::fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(fs::io)?;
    std::fs::File::open(path.parent().ok_or_else(binding)?)
        .and_then(|f| f.sync_all())
        .map_err(fs::io)
}
fn binding() -> Error {
    Error::new(
        ErrorCode::ProjectBindingMismatch,
        "Host descriptor no longer routes to its initialized project.",
    )
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    context_id: Uuid,
    incarnation: Uuid,
    project_id: Uuid,
    host: HostKind,
    host_session_id: String,
    host_version: String,
    os_profile: String,
    cwd_bytes: Vec<u8>,
}
impl From<&Context> for Stored {
    fn from(c: &Context) -> Self {
        Self {
            version: 1,
            context_id: c.context_id,
            incarnation: c.incarnation,
            project_id: c.project_id,
            host: c.host,
            host_session_id: c.host_session_id.clone(),
            host_version: c.host_version.clone(),
            os_profile: c.os_profile.clone(),
            cwd_bytes: c.working_directory.as_os_str().as_bytes().to_vec(),
        }
    }
}
impl Stored {
    fn context(self, id: Uuid) -> Result<Context> {
        if self.version != 1 {
            return Err(Error::new(
                ErrorCode::UnsupportedProtocol,
                "Host descriptor version is unsupported.",
            ));
        }
        let path = PathBuf::from(OsString::from_vec(self.cwd_bytes.clone()));
        if self.context_id != id
            || self.cwd_bytes.len() > 16_384
            || self.cwd_bytes.contains(&0)
            || !path.is_absolute()
            || self.host_session_id.is_empty()
            || self.host_session_id.len() > 1024
            || self.host_version.is_empty()
            || self.host_version.len() > 128
            || self.os_profile.is_empty()
            || self.os_profile.len() > 128
        {
            return Err(fs::error(
                "Host descriptor violates its bounded identity format.",
            ));
        }
        Ok(Context {
            context_id: id,
            incarnation: self.incarnation,
            project_id: self.project_id,
            host: self.host,
            host_session_id: self.host_session_id,
            host_version: self.host_version,
            os_profile: self.os_profile,
            working_directory: path,
        })
    }
}
