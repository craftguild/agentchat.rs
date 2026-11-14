//! Exact native-owner process observations, never conversation identity alone.
//!
//! A trusted native lifecycle must associate this token with its exact root and
//! observer. Rechecking a live PID cannot establish the current root after clear,
//! loaded-observer health, host phase, participation or automatic-wake admission.

mod ancestry;
mod birth;
mod continuity;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
pub use birth::BirthState;
pub use continuity::ContinuityState;

use agentchat_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::Metadata,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Immutable kernel identity of one explicitly identified native owner process.
/// Serialization is local routing metadata, not cross-user authentication.
/// Rechecks reject missing, unreadable, replaced and malformed observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pid: u32,
    boot: Uuid,
    start: Start,
    uid: u32,
    image: Image,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
enum Start {
    Linux { ticks: u64 },
    Macos { seconds: u64, microseconds: u64 },
}
#[derive(Debug, Clone, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    // Preserve native Unix executable bytes; never decode a Linux path lossily.
    path: Vec<u8>,
    device: u64,
    inode: u64,
}
impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        // Retain device as launch evidence/wire data, never a startup gate.
        self.path == other.path && self.inode == other.inode
    }
}
impl Image {
    fn new(path: &Path, metadata: Metadata) -> Result<Self> {
        if !path.is_absolute() || path.as_os_str().len() > 16_384 || !metadata.is_file() {
            return Err(unavailable());
        }
        Ok(Self {
            path: path.as_os_str().as_bytes().to_vec(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}
impl ProcessIdentity {
    /// Observe one known process without inspecting arguments or environment.
    /// Only the current OS user's non-zombie process can produce a token. This
    /// does not discover a conversation or assert host policy/admission.
    pub fn capture(pid: u32) -> Result<Self> {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(unavailable());
        }
        #[cfg(target_os = "linux")]
        let token = linux::observe(pid)?;
        #[cfg(target_os = "macos")]
        let token = macos::observe(pid)?;
        if token.uid != rustix::process::geteuid().as_raw() {
            return Err(unavailable());
        }
        Ok(token)
    }
    /// Capture the direct native callback owner, checking the expected executable.
    /// Call only inside the real native helper boundary; arguments/PID alone do
    /// not authenticate a root. Reparenting during capture refuses the result.
    pub fn parent(expected_executable: &Path) -> Result<Self> {
        let parent = rustix::process::getppid().ok_or_else(unavailable)?;
        let token = Self::capture(parent.as_raw_pid() as u32)?;
        let expected = expected_executable
            .canonicalize()
            .map_err(|_| unavailable())?;
        let image = Image::new(&expected, expected.metadata().map_err(|_| unavailable())?)?;
        if token.image != image || rustix::process::getppid() != Some(parent) {
            return Err(unavailable());
        }
        Ok(token)
    }
    /// Find the nearest exact native image in this process's kernel ancestry.
    /// At most eight parent links are examined and all captured links are
    /// rechecked before return. Use this to pin a physical MCP process owner,
    /// never to infer a conversation root or authorize waking it. A trusted
    /// lifecycle and fresh native root/phase proof are still required.
    pub fn ancestor(expected_executable: &Path) -> Result<Self> {
        let expected = expected_executable
            .canonicalize()
            .map_err(|_| unavailable())?;
        let image = Image::new(&expected, expected.metadata().map_err(|_| unavailable())?)?;
        let mut chain = vec![Self::capture(std::process::id())?];
        for _ in 0..8 {
            let child = chain.last().ok_or_else(unavailable)?;
            let parent = Self::capture(parent_pid(child.pid)?)?;
            if !child.is_current() || parent_pid(child.pid)? != parent.pid {
                return Err(unavailable());
            }
            let found = parent.image == image;
            chain.push(parent);
            if found {
                for pair in chain.windows(2) {
                    if !pair[0].is_current()
                        || !pair[1].is_current()
                        || parent_pid(pair[0].pid)? != pair[1].pid
                    {
                        return Err(unavailable());
                    }
                }
                return chain.pop().ok_or_else(unavailable);
            }
        }
        Err(unavailable())
    }
    /// Kernel PID, useful only with the complete captured identity and root proof.
    pub fn pid(&self) -> u32 {
        self.pid
    }
    /// Initial path/inode and kernel identity match the captured process token;
    /// the diagnostic device number is deliberately excluded on both OSes.
    /// This deliberately rejects installed-image replacement; ongoing callers
    /// use continuity_state instead of opening the current executable path.
    /// False also covers observation errors; an unreadable owner is never live.
    /// Loaded host metadata and other live helpers cannot substitute for it.
    pub fn is_current(&self) -> bool {
        if self.image.path.is_empty()
            || self.image.path.len() > 16_384
            || self.image.path.contains(&0)
        {
            return false;
        }
        Self::capture(self.pid).is_ok_and(|now| now == *self)
    }
    /// Observe the original process's boot/PID/start/user without executable
    /// path, inode, device or filesystem lookup. This is ongoing continuity,
    /// not initial image selection, conversation authority or same-birth exec
    /// detection. Unknown must suspend; only positive Gone permits retirement.
    pub fn continuity_state(&self) -> ContinuityState {
        continuity::observe(self)
    }
    /// Whether original-process continuity is positively current. False also
    /// includes uncertainty; use continuity_state before irreversible retirement.
    pub fn is_continuing(&self) -> bool {
        self.continuity_state() == ContinuityState::Current
    }
    /// Observe the original physical birth for retention decisions. This ignores
    /// image/UID changes, which can be reversed during the same process lifetime.
    /// Unknown observations must preserve metadata. Even `Gone` does not
    /// authenticate serialized input: cleanup must separately validate the saved
    /// owner record, namespace/inode and lifecycle fences before deleting files.
    /// No result establishes host connection, current root or wake permission.
    pub fn birth_state(&self) -> BirthState {
        birth::observe(self)
    }
    /// Native image path without lossy Unicode conversion.
    pub fn executable(&self) -> PathBuf {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(self.image.path.clone()).into()
    }
    /// Compare this captured image against an independently retained executable
    /// path/file identity. This performs no lookup, refresh, root inference or
    /// authority renewal; a caller can pin metadata before spawning a child.
    pub fn matches_executable(&self, path: &Path, metadata: Metadata) -> bool {
        Image::new(path, metadata).is_ok_and(|expected| self.image == expected)
    }
}
fn parent_pid(pid: u32) -> Result<u32> {
    #[cfg(target_os = "linux")]
    let parent = linux::parent_pid(pid)?;
    #[cfg(target_os = "macos")]
    let parent = macos::parent_pid(pid)?;
    if parent == 0 || parent > i32::MAX as u32 {
        return Err(unavailable());
    }
    Ok(parent)
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The exact native process owner is unavailable or changed.",
    )
}
