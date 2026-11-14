//! Journaled inode inventory; retries may observe only original nodes or absence.

use super::{conflict, io, io::Directory};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

const MAX_NODES: usize = 65_536;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    kind: u8,
    size: u64,
    modified: (i64, i64),
}
impl Stamp {
    fn read(path: &Path) -> Result<Option<Self>> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        };
        let kind = if meta.is_dir() {
            0
        } else if meta.is_file() {
            1
        } else if meta.file_type().is_socket() {
            2
        } else {
            return Err(conflict(
                "Removal target contains a symbolic link or unsupported filesystem type.",
            ));
        };
        if meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o022 != 0
            || kind != 0 && meta.nlink() != 1
        {
            return Err(conflict(
                "Removal target contains foreign-owned, writable or multiply linked data.",
            ));
        }
        Ok(Some(Self {
            device: meta.dev(),
            inode: meta.ino(),
            mode: meta.mode() & 0o777,
            kind,
            size: if kind == 0 { 0 } else { meta.len() },
            modified: if kind == 0 {
                (0, 0)
            } else {
                (meta.mtime(), meta.mtime_nsec())
            },
        }))
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    relative: Vec<u8>,
    stamp: Stamp,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tree {
    pub component: String,
    #[serde(with = "super::native")]
    pub path: PathBuf,
    nodes: Vec<Node>,
    file: bool,
}

impl Tree {
    pub fn capture(component: &str, path: PathBuf) -> Result<Self> {
        super::io::checked(&path)?;
        let mut value = Self {
            component: component.into(),
            path,
            nodes: Vec::new(),
            file: false,
        };
        value.walk(Path::new(""))?;
        Ok(value)
    }
    pub fn capture_file(component: &str, path: PathBuf) -> Result<Self> {
        let mut value = Self::capture(component, path)?;
        value.file = true;
        value.validate_format()?;
        Ok(value)
    }
    pub fn capture_reference(path: PathBuf) -> Result<Self> {
        // A reference grants authority over its small metadata directory, not
        // an accidentally overlapping state root or unrelated profile files.
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
            Ok(_) => {
                let held = Directory::open(&path)?;
                for entry in fs::read_dir(&path).map_err(io)? {
                    let entry = entry.map_err(io)?;
                    if ![
                        std::ffi::OsStr::new("project.json"),
                        std::ffi::OsStr::new("init.lock"),
                    ]
                    .contains(&entry.file_name().as_os_str())
                    {
                        return Err(conflict(
                            "Project reference directory contains unrecognized members or overlapping state; all targets were preserved.",
                        ));
                    }
                }
                held.current()?;
            }
        }
        Self::capture("project_reference", path)
    }
    pub fn is_file(&self) -> bool {
        self.file
    }
    pub fn reject_reference_overlap(&self) -> Result<()> {
        for node in &self.nodes {
            let path = relative(&node.relative)?;
            if path.file_name() == Some(std::ffi::OsStr::new("project.json"))
                && (path.parent() == Some(Path::new(""))
                    || path
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|name| name == ".agentchat" || name == "agentchat"))
            {
                return Err(conflict(
                    "Selected global state contains a project reference destination; all targets were preserved.",
                ));
            }
        }
        Ok(())
    }
    pub fn present(&self) -> bool {
        !self.nodes.is_empty()
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn require_absent(&self) -> Result<()> {
        match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(e)),
            Ok(_) => Err(conflict(
                "A completed removal destination was replaced; the replacement was preserved.",
            )),
        }
    }
    fn walk(&mut self, relative: &Path) -> Result<()> {
        if self.nodes.len() >= MAX_NODES {
            return Err(conflict(
                "Removal target exceeds its bounded inode inventory.",
            ));
        }
        let path = if relative.as_os_str().is_empty() {
            self.path.clone()
        } else {
            self.path.join(relative)
        };
        let Some(stamp) = Stamp::read(&path)? else {
            return Ok(());
        };
        let directory = stamp.kind == 0;
        // Another root's stable fence/recovery directory may be nested here.
        // Removing it would admit replacement startup and erase retired keys.
        if directory
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix(".agentchat-lifecycle-"))
                .is_some_and(|key| key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(conflict(
                "Removal overlaps another lifecycle/recovery directory; all targets were preserved. Select a disjoint state root.",
            ));
        }
        self.nodes.push(Node {
            relative: relative.as_os_str().as_encoded_bytes().to_vec(),
            stamp,
        });
        if directory {
            let held = Directory::open(&path)?;
            let mut entries = fs::read_dir(&path)
                .map_err(io)?
                .map(|entry| entry.map(|e| e.file_name()).map_err(io))
                .collect::<Result<Vec<_>>>()?;
            entries.sort();
            held.current()?;
            for name in entries {
                self.walk(&relative.join(name))?;
            }
            held.current()?;
        }
        Ok(())
    }
    pub fn validate_format(&self) -> Result<()> {
        super::io::checked(&self.path)?;
        if self.nodes.len() > MAX_NODES {
            return Err(conflict("Invalid removal tree budget."));
        }
        let mut seen = BTreeMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let relative = relative(&node.relative)?;
            if (index == 0 && (!node.relative.is_empty() || node.stamp.kind != u8::from(self.file)))
                || seen
                    .insert(node.relative.clone(), node.stamp.kind)
                    .is_some()
                || node.stamp.kind > 2
                || node.stamp.mode & !0o777 != 0
            {
                return Err(conflict("Invalid removal inode inventory."));
            }
            if index > 0
                && seen.get(
                    relative
                        .parent()
                        .unwrap_or(Path::new(""))
                        .as_os_str()
                        .as_encoded_bytes(),
                ) != Some(&0)
            {
                return Err(conflict(
                    "Removal inventory crosses a non-directory parent.",
                ));
            }
        }
        Ok(())
    }
    pub fn validate_remaining(&self) -> Result<()> {
        self.validate_format()?;
        let current = Self::capture(&self.component, self.path.clone())?;
        let planned = self
            .nodes
            .iter()
            .map(|n| (&n.relative, &n.stamp))
            .collect::<BTreeMap<_, _>>();
        for node in current.nodes {
            if planned.get(&node.relative).copied() != Some(&node.stamp) {
                return Err(conflict(
                    "Removal destination changed after planning; remaining contents were preserved.",
                ));
            }
        }
        Ok(())
    }
    /// Parents are opened before unlink and every node is rechecked. Unlink is
    /// relative to that held FD, so an ancestor substitution cannot escape.
    /// Cooperating writers are fenced; noncooperating writers cannot be given
    /// a portable compare-and-unlink atomicity guarantee across the last syscall.
    pub fn apply(&self, mut removed: impl FnMut(usize) -> Result<()>) -> Result<()> {
        self.validate_remaining()?;
        for (index, node) in self.nodes.iter().enumerate().rev() {
            let relative = relative(&node.relative)?;
            let path = if relative.as_os_str().is_empty() {
                self.path.clone()
            } else {
                self.path.join(relative)
            };
            let Some(actual) = Stamp::read(&path)? else {
                continue;
            };
            if actual != node.stamp {
                return Err(conflict(
                    "Removal node preimage changed immediately before deletion.",
                ));
            }
            let parent = Directory::open(
                path.parent()
                    .ok_or_else(|| conflict("Removal target has no parent."))?,
            )?;
            if Stamp::read(&path)?.as_ref() != Some(&node.stamp) {
                return Err(conflict("Removal node identity changed before unlink."));
            }
            parent.unlink(
                path.file_name()
                    .ok_or_else(|| conflict("Missing removal filename."))?,
                node.stamp.kind == 0,
            )?;
            removed(index)?;
        }
        // Recovery after unlink but before parent sync must close that durable
        // boundary even when the last root is already absent on this attempt.
        if let Some(parent) = self.path.parent().filter(|p| p.is_dir()) {
            Directory::open(parent)?.sync()?;
        }
        self.require_absent()
    }
}

fn relative(bytes: &[u8]) -> Result<PathBuf> {
    let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    if bytes.contains(&0)
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(conflict("Invalid relative path in removal inventory."));
    }
    Ok(path)
}
