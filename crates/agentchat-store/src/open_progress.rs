//! Bounded synchronous progress for operators observing an exclusive schema open.
use std::path::PathBuf;

/// A storage-open boundary, observed while the OS migration lock is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenStage {
    /// Compatibility and checksums passed on the initial read-only connection.
    /// None means the database is empty and will be initialized.
    ReadOnlyChecked {
        /// Supported existing schema version, if initialized.
        schema_version: Option<i32>,
    },
    /// Original-schema backup is validated, synchronized and published.
    BackupPublished {
        /// Source schema version retained in this backup.
        schema_version: i32,
        /// Exact owned backup path, retained even if later migration rolls back.
        destination: PathBuf,
    },
    /// Official migrator DDL/tracking have run inside the uncommitted transaction.
    SchemaApplied,
    /// New application checksum rows are written but still uncommitted.
    ChecksumsWritten,
    /// Project version metadata is written but still uncommitted.
    MetadataWritten,
    /// Schema/checksum, integrity and foreign-key validation passed before commit.
    UpgradeValidated,
    /// About to commit initialization or the complete product upgrade.
    BeforeCommit,
    /// Initialization/upgrade commit succeeded; the migration lock is still held.
    Committed,
}

/// Optional diagnostics sink for a guarded schema open or upgrade.
///
/// Calls are synchronous under the migration lock and may hold an immediate
/// transaction. Keep them bounded; do not reenter storage, change the database,
/// alter backups or introduce a second migration policy. Observation cannot
/// acknowledge a business operation or turn an uncommitted stage into success.
pub trait OpenProgress: Send + Sync {
    /// Observe one precise durability boundary without modifying its state.
    fn reached(&self, stage: OpenStage);
}
pub(crate) struct Silent;
impl OpenProgress for Silent {
    fn reached(&self, _: OpenStage) {}
}
