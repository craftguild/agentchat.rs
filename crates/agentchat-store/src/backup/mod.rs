//! ORM-only logical online backup of the current fresh-project Dispatch format.

mod files;
mod progress;
use crate::{
    Store, connection,
    entities::*,
    error::{database, integrity},
    schema,
};
use agentchat_core::{Result, sequence::Cursor};
use files::Stage;
pub use progress::{BackupProgress, BackupStage, BackupTable};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait,
    IntoActiveModel, Iterable, PrimaryKeyToColumn, QueryFilter, QueryOrder, QuerySelect, Select,
    Set, SqliteTransactionMode, TransactionOptions, TransactionTrait,
    sea_query::{Alias, Expr, ExprTrait, Func, LikeExpr, Order, Query},
};
use sea_orm_migration::seaql_migrations;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Validated snapshot backup, published at a previously absent destination.
#[derive(Debug, Clone)]
pub struct BackupReport {
    /// Canonical parent plus the requested new filename.
    pub destination: PathBuf,
    /// Last committed source event in the captured snapshot.
    pub head: Cursor,
    /// Total retained rows copied, including metadata and migration history.
    pub rows: u64,
}
impl Store {
    /// Copy one consistent read-only snapshot using SeaORM exclusively.
    ///
    /// Canonical schema equality is checked before copying; unknown schema
    /// objects/columns are refused rather than silently omitted. Source writers
    /// remain available under WAL. Publication never overwrites any filesystem
    /// object, and happens only after destination integrity/checksum validation
    /// and closing both destination pools so its WAL has been checkpointed.
    pub async fn backup(&self, destination: impl AsRef<Path>) -> Result<BackupReport> {
        self.backup_observed(destination, Arc::new(progress::Silent))
            .await
    }

    /// Copy with bounded progress at chunk and publication durability boundaries.
    ///
    /// This invokes the same snapshot, validation and publication path as
    /// `backup`. The owned sink may also run on its blocking filesystem worker.
    pub async fn backup_observed(
        &self,
        destination: impl AsRef<Path>,
        progress: Arc<dyn BackupProgress>,
    ) -> Result<BackupReport> {
        let destination = destination.as_ref().to_owned();
        let stage = tokio::task::spawn_blocking(move || Stage::create(&destination))
            .await
            .map_err(|_| integrity("Backup staging worker failed."))??;
        progress.reached(BackupStage::StagingCreated);
        let source = self.reader.begin().await.map_err(database)?;
        let version = connection::preflight(&source, &self.identity)
            .await?
            .ok_or_else(|| integrity("Backup snapshot is missing its initialized schema."))?;
        let target = target(&stage.database(), &self.identity, version).await?;
        // Preflight anchors the source snapshot before catalog comparison/copy.
        if catalog(&source).await? != catalog(&target.db).await? {
            return Err(integrity(
                "Backup refuses schema objects outside the canonical supported migration.",
            ));
        }
        let head = event::Entity::find()
            .order_by_desc(event::Column::Seq)
            .one(&source)
            .await
            .map_err(database)?
            .map_or(Ok(Cursor::START), |event| Cursor::new(event.seq))?;
        progress.reached(BackupStage::SnapshotAnchored {
            schema_version: version,
            head,
        });
        let destination = target.db.begin().await.map_err(database)?;
        project_receipt_settings::Entity::delete_many()
            .exec(&destination)
            .await
            .map_err(database)?;
        project::Entity::delete_many()
            .exec(&destination)
            .await
            .map_err(database)?;
        migration::Entity::delete_many()
            .exec(&destination)
            .await
            .map_err(database)?;
        seaql_migrations::Entity::delete_many()
            .exec(&destination)
            .await
            .map_err(database)?;
        let mut rows = 0;
        macro_rules! copy {
            ($entity:path, $phase:ident) => {
                rows += copy(
                    <$entity>::find(),
                    &source,
                    &destination,
                    BackupTable::$phase,
                    progress.as_ref(),
                )
                .await?;
            };
        }
        copy!(project::Entity, Project);
        copy!(project_receipt_settings::Entity, ProjectReceiptSettings);
        copy!(migration::Entity, Migrations);
        copy!(seaql_migrations::Entity, SeaOrmMigrations);
        // Private skeletons break the immediate FK cycle. Neither an incomplete
        // audit envelope nor Header is exposed by the destination publication.
        events(&source, &destination, true, progress.as_ref()).await?;
        copy!(actor::Entity, Actors);
        copy!(launch::Entity, Launches);
        copy!(channel::Entity, Channels);
        copy!(project_default_channel::Entity, ProjectDefaultChannels);
        copy!(session::Entity, Sessions);
        copy!(membership::Entity, Memberships);
        copy!(dispatch::Entity, Dispatches);
        rows += copy(
            message::Entity::find().filter(message::Column::RootMessageId.is_null()),
            &source,
            &destination,
            BackupTable::MessageRoots,
            progress.as_ref(),
        )
        .await?;
        rows += copy(
            message::Entity::find().filter(message::Column::RootMessageId.is_not_null()),
            &source,
            &destination,
            BackupTable::MessageReplies,
            progress.as_ref(),
        )
        .await?;
        copy!(notice::Entity, DispatchNotices);
        rows += events(&source, &destination, false, progress.as_ref()).await?;
        copy!(mention::Entity, Mentions);
        copy!(receipt::Entity, ReadReceipts);
        copy!(wake_batch::Entity, WakeBatches);
        copy!(delivery::Entity, Deliveries);
        copy!(delivery_transition::Entity, DeliveryStateTransitions);
        copy!(delivery_timeout_notice::Entity, DeliveryTimeoutNotices);
        copy!(message_stream::Entity, MessageStreams);
        copy!(reservation::Entity, Reservations);
        copy!(reservation_pattern::Entity, ReservationPatterns);
        copy!(idempotency::Entity, Idempotency);
        let counters = sequences(&source, &destination).await?;
        rows += counters;
        if counters > 0 {
            progress.reached(BackupStage::CopyChunk {
                table: BackupTable::Sequences,
                rows: counters,
            });
        }
        destination.commit().await.map_err(database)?;
        progress.reached(BackupStage::DestinationCommitted);
        source.commit().await.map_err(database)?;
        target.integrity_check().await?;
        progress.reached(BackupStage::Validated);
        target.close().await?;
        progress.reached(BackupStage::Closed);
        let report = BackupReport {
            destination: stage.destination.clone(),
            head,
            rows,
        };
        tokio::task::spawn_blocking(move || stage.publish(progress.as_ref()))
            .await
            .map_err(|_| integrity("Backup publication worker failed."))??;
        Ok(report)
    }
}
async fn target(
    path: &Path,
    identity: &agentchat_core::project::ProjectIdentity,
    version: i32,
) -> Result<Store> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(crate::error::io)?;
    let db = connection::connect_for_schema(path).await?;
    let tx = db
        .begin_with_options(TransactionOptions {
            sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
            ..Default::default()
        })
        .await
        .map_err(database)?;
    // A pre-upgrade backup must keep its original schema and both ledgers,
    // rather than migrating the backup into the schema it is meant to precede.
    schema::bootstrap(&tx, identity, version).await?;
    tx.commit().await.map_err(database)?;
    db.close().await.map_err(database)?;
    let db = connection::connect(path, false).await?;
    let reader = connection::connect(path, true).await?;
    Ok(Store {
        db,
        reader,
        identity: identity.clone(),
    })
}
async fn sequences(source: &DatabaseTransaction, target: &DatabaseTransaction) -> Result<u64> {
    let records = source
        .query_all(
            &Query::select()
                .columns([Alias::new("name"), Alias::new("seq")])
                .from(Alias::new("sqlite_sequence"))
                .limit(17)
                .to_owned(),
        )
        .await
        .map_err(database)?;
    if records.len() > 16 {
        return Err(integrity("Unexpected number of SQLite sequence counters."));
    }
    target
        .execute(
            &Query::delete()
                .from_table(Alias::new("sqlite_sequence"))
                .to_owned(),
        )
        .await
        .map_err(database)?;
    for record in &records {
        let name: String = record.try_get("", "name").map_err(database)?;
        let value: i64 = record.try_get("", "seq").map_err(database)?;
        if name != "events" || value < 0 {
            return Err(integrity("Unsupported SQLite sequence counter."));
        }
        target
            .execute(
                &Query::insert()
                    .into_table(Alias::new("sqlite_sequence"))
                    .columns([Alias::new("name"), Alias::new("seq")])
                    .values_panic([name.into(), value.into()])
                    .to_owned(),
            )
            .await
            .map_err(database)?;
    }
    Ok(records.len() as u64)
}
async fn copy<E>(
    mut query: Select<E>,
    source: &DatabaseTransaction,
    target: &DatabaseTransaction,
    table: BackupTable,
    progress: &dyn BackupProgress,
) -> Result<u64>
where
    E: EntityTrait,
    E::Model: IntoActiveModel<E::ActiveModel>,
{
    for key in E::PrimaryKey::iter() {
        query = query.order_by_asc(key.into_column());
    }
    let mut offset = 0;
    loop {
        // Sixteen maximum-size message rows keep each copy chunk near 1 MiB.
        let records = query
            .clone()
            .offset(offset)
            .limit(16)
            .all(source)
            .await
            .map_err(database)?;
        let count = records.len() as u64;
        for record in records {
            E::insert(record.into_active_model())
                .exec(target)
                .await
                .map_err(database)?;
        }
        offset += count;
        if count > 0 {
            progress.reached(BackupStage::CopyChunk {
                table,
                rows: offset,
            });
        }
        if count < 16 {
            return Ok(offset);
        }
    }
}
async fn events(
    source: &DatabaseTransaction,
    target: &DatabaseTransaction,
    staging: bool,
    progress: &dyn BackupProgress,
) -> Result<u64> {
    let mut after = 0;
    let mut count = 0;
    loop {
        let events = event::Entity::find()
            .filter(event::Column::Seq.gt(after))
            .order_by_asc(event::Column::Seq)
            .limit(16)
            .all(source)
            .await
            .map_err(database)?;
        if events.is_empty() {
            return Ok(count);
        }
        for row in events {
            if !row.sealed {
                return Err(integrity("Backup snapshot contains an unpublished event."));
            }
            after = row.seq;
            count += 1;
            let mut active = row.into_active_model().reset_all();
            if staging {
                active.actor_id = Set(None);
                active.channel_id = Set(None);
                active.message_id = Set(None);
                active.sealed = Set(false);
                active.insert(target).await.map_err(database)?;
            } else {
                active.update(target).await.map_err(database)?;
            }
        }
        progress.reached(BackupStage::CopyChunk {
            table: if staging {
                BackupTable::EventSkeletons
            } else {
                BackupTable::Events
            },
            rows: count,
        });
    }
}
async fn catalog<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(String, String, String, Option<String>)>> {
    let mut query = Query::select();
    // Unknown catalog objects cannot allocate unbounded names/DDL in the
    // driver. Truncation and excess rows necessarily differ from our catalog.
    for (column, bound) in [
        ("type", 32),
        ("name", 256),
        ("tbl_name", 256),
        ("sql", 65_537),
    ] {
        query.expr_as(
            Func::cust(Alias::new("substr"))
                .arg(Expr::col(Alias::new(column)))
                .arg(1)
                .arg(bound),
            Alias::new(column),
        );
    }
    let query = query
        .from(Alias::new("sqlite_schema"))
        .and_where(Expr::col(Alias::new("name")).not_like(LikeExpr::new(r"sqlite\_%").escape('\\')))
        .order_by(Alias::new("name"), Order::Asc)
        .limit(129)
        .to_owned();
    let rows = db.query_all(&query).await.map_err(database)?;
    if rows.len() > 128 {
        return Err(integrity("Backup catalog exceeds the supported bound."));
    }
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get("", "type").map_err(database)?,
                row.try_get("", "name").map_err(database)?,
                row.try_get("", "tbl_name").map_err(database)?,
                row.try_get("", "sql").map_err(database)?,
            ))
        })
        .collect()
}
