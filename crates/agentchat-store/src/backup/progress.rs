//! Closed progress vocabulary for observing a backup without changing policy.

use agentchat_core::sequence::Cursor;

/// A fixed logical copy phase; rows remain private until complete publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupTable {
    /// Project identity and source schema metadata.
    Project,
    /// Application migration checksums and timestamps.
    Migrations,
    /// SeaORM's original named migration history.
    SeaOrmMigrations,
    /// Temporary unpublished event skeletons for immediate foreign-key cycles.
    EventSkeletons,
    /// Retained participant identities.
    Agents,
    /// Common native/user participation identities in schema v11 and later.
    Actors,
    /// Original launch generations and terminal retirement records.
    Launches,
    /// Retained channels.
    Channels,
    /// Ordered project default-channel policy rows.
    ProjectDefaultChannels,
    /// Effective receipt policy, retaining explicit false and captured timeout.
    ProjectReceiptSettings,
    /// Exact host session bindings.
    Sessions,
    /// All-channel membership rows.
    Memberships,
    /// Immutable Message/Notice sending Headers.
    Dispatches,
    /// Typed system-notification payloads owned by Headers.
    DispatchNotices,
    /// Append-only recipient state transitions and frozen first facts.
    DeliveryStateTransitions,
    /// Root message bodies, before replies reference them.
    MessageRoots,
    /// Reply message bodies.
    MessageReplies,
    /// Complete sealed event envelopes replacing skeletons.
    Events,
    /// Frozen mention resolution.
    Mentions,
    /// Explicit read receipts.
    ReadReceipts,
    /// Waiting and terminal individual read watches.
    ReadConfirmations,
    /// Original-message timeout notification coalescing identities.
    DeliveryTimeoutNotices,
    /// Retained native delivery batches.
    WakeBatches,
    /// Durable original-recipient rows and lifecycle history.
    Deliveries,
    /// Native full-body cursors and failed/ambiguous submission heads.
    MessageStreams,
    /// Retained cooperative reservation groups.
    Reservations,
    /// Individual patterns belonging to reservation groups.
    ReservationPatterns,
    /// Stable successful mutation results.
    Idempotency,
    /// SQLite event autoincrement high-water mark.
    Sequences,
}

/// A precise backup boundary, not permission to expose private partial data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStage {
    /// A new private staging directory exists; no source snapshot is held yet.
    StagingCreated,
    /// Read-only compatibility/catalog checks and the captured head are known.
    SnapshotAnchored {
        /// Original schema version, also used for the destination migrator.
        schema_version: i32,
        /// Last source event in the immutable read snapshot.
        head: Cursor,
    },
    /// A bounded nonempty chunk has been copied inside the private transaction.
    CopyChunk {
        /// Fixed copy phase, with skeleton insertion distinct from final events.
        table: BackupTable,
        /// Cumulative rows in this phase, including the just-completed chunk.
        rows: u64,
    },
    /// All destination rows and counters committed; validation is still pending.
    DestinationCommitted,
    /// Destination integrity, foreign keys and migration checksums passed.
    Validated,
    /// Both destination pools closed, including their WAL checkpoint.
    Closed,
    /// The closed destination's main database file was synchronized.
    FileSynced,
    /// Create-new linking succeeded; parent-directory synchronization is pending.
    Linked,
    /// Destination parent was synchronized; complete publication is durable.
    DirectorySynced,
}

/// Optional bounded synchronous diagnostics for the ordinary backup path.
///
/// Calls may run on a blocking publication worker or while a read snapshot and
/// destination transaction are held. Do not reenter storage, alter backup/source
/// state, panic, or infer success from an intermediate boundary. The sink cannot
/// change copying, validation, ownership or create-new publication policy.
pub trait BackupProgress: Send + Sync {
    /// Observe one boundary without modifying backup or application state.
    fn reached(&self, stage: BackupStage);
}

pub(super) struct Silent;
impl BackupProgress for Silent {
    fn reached(&self, _: BackupStage) {}
}
