//! Lexical coordination locks; data-writer retention has a separate lifetime.
use std::{
    fs::File,
    ops::{Deref, DerefMut},
};

/// One acquired coordination lock, released when its critical section ends.
/// Closing alone can retain flock through a descriptor inherited by an unrelated
/// fork until that child execs. Explicit unlock ends only this acquired lock;
/// the owned FD remains open until Drop finishes. Never use this wrapper for a
/// staged data-writer lock whose retention intentionally follows every FD copy.
pub(crate) struct ScopedLock(File);
impl ScopedLock {
    /// Own an exact file description immediately after successful acquisition.
    /// This neither acquires a lock nor authenticates an inode or its contents.
    pub(crate) fn acquired(file: File) -> Self {
        Self(file)
    }
}
impl Deref for ScopedLock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}
impl DerefMut for ScopedLock {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.0
    }
}
impl Drop for ScopedLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

#[cfg(test)]
#[path = "../tests/unit/scoped_lock.rs"]
mod tests;
