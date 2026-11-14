//! SQLite persistence adapter for AgentChat application transactions.

mod aggregate_integrity;
mod backup;
mod connection;
mod decode;
mod delivery_history;
mod diagnostics;
mod dispatches;
mod entities;
mod error;
pub mod migrations;
mod open_progress;
mod queue;
mod read;
mod receipt_settings;
mod schema;
mod scope;
mod transaction;
mod upgrade;
mod wake_candidates;

pub use backup::{BackupProgress, BackupReport, BackupStage, BackupTable};
pub use diagnostics::ConnectionSettings;
pub use open_progress::{OpenProgress, OpenStage};

/// Latest supported named schema; prior migration sources remain immutable.
pub const SCHEMA_VERSION: i32 = schema::VERSION;

use std::{
    fs::{DirBuilder, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

use agentchat_core::{Result, project::ProjectIdentity};
use fs2::FileExt;
use sea_orm::{DatabaseConnection, SqliteTransactionMode, TransactionOptions, TransactionTrait};

/// An isolated project database. ORM connections are not exposed to transports.
pub struct Store {
    db: DatabaseConnection,
    reader: DatabaseConnection,
    identity: ProjectIdentity,
}

impl Store {
    /// Open or initialize a project database under an OS-held migration lock.
    ///
    /// Existing database compatibility is checked read-only before WAL/write
    /// pragmas. All connection work is asynchronous through SeaORM's driver.
    pub async fn open(path: impl AsRef<Path>, identity: ProjectIdentity) -> Result<Self> {
        Self::open_observed(path, identity, &open_progress::Silent).await
    }

    /// Open with bounded diagnostics at schema/backup durability boundaries.
    /// The same guards, named migrations and transaction policy as `open` apply.
    pub async fn open_observed(
        path: impl AsRef<Path>,
        identity: ProjectIdentity,
        progress: &dyn OpenProgress,
    ) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let lock_path = path.with_extension("migration.lock");
        let db_path = path.clone();
        let lock = tokio::task::spawn_blocking(move || -> Result<File> {
            if let Some(parent) = db_path.parent() {
                DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .map_err(error::io)?;
            }
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(lock_path)
                .map_err(error::io)?;
            FileExt::lock_exclusive(&lock).map_err(error::io)?;
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&db_path)
            {
                Ok(file) => file.sync_all().map_err(error::io)?,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(error::io(e)),
            }
            Ok(lock)
        })
        .await
        .map_err(|_| error::integrity("Migration lock worker failed."))??;
        let read = connection::connect(&path, true).await?;
        let preflight = connection::preflight(&read, &identity).await;
        let version = preflight?;
        progress.reached(OpenStage::ReadOnlyChecked {
            schema_version: version,
        });
        let migrating = version != Some(schema::VERSION);
        let mut db = if migrating {
            connection::connect_for_schema(&path).await?
        } else {
            connection::connect(&path, false).await?
        };
        if migrating {
            let tx = db
                .begin_with_options(TransactionOptions {
                    sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
                    ..Default::default()
                })
                .await
                .map_err(error::database)?;
            if let Some(version) = version {
                upgrade::apply(&db, &read, &tx, &path, &identity, version, progress).await?;
            } else {
                schema::bootstrap(&tx, &identity, schema::VERSION).await?;
                diagnostics::check(&tx).await?;
            }
            progress.reached(OpenStage::BeforeCommit);
            tx.commit().await.map_err(error::database)?;
            progress.reached(OpenStage::Committed);
            db.close().await.map_err(error::database)?;
            db = connection::connect(&path, false).await?;
        }
        read.close().await.map_err(error::database)?;
        drop(lock);
        let reader = connection::connect(&path, true).await?;
        Ok(Self {
            db,
            reader,
            identity,
        })
    }

    /// The expected project identity, validated against database metadata.
    pub fn identity(&self) -> &ProjectIdentity {
        &self.identity
    }

    /// Actual linked SQLite version, checked against the required WAL fix.
    pub async fn sqlite_version(&self) -> Result<String> {
        connection::sqlite_version(&self.db).await
    }

    /// Read the actual storage connection's integrity-related SQLite settings.
    pub async fn connection_settings(&self) -> Result<ConnectionSettings> {
        diagnostics::settings(&self.db).await
    }

    /// Check database integrity, foreign keys and migration checksums without repair.
    pub async fn integrity_check(&self) -> Result<()> {
        let snapshot = self.reader.begin().await.map_err(error::database)?;
        let result = async {
            diagnostics::check(&snapshot).await?;
            schema::validate(&snapshot).await?;
            aggregate_integrity::check(&snapshot).await
        }
        .await;
        snapshot.rollback().await.map_err(error::database)?;
        result
    }

    /// Close the connection pool before moving or inspecting disposable files.
    pub async fn close(self) -> Result<()> {
        self.reader.close().await.map_err(error::database)?;
        self.db.close().await.map_err(error::database)
    }
}
