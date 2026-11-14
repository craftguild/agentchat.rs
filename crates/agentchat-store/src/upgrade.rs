//! Backup precedes DDL while the source immediate lock excludes other writers.

use crate::{
    OpenProgress, OpenStage, Store, connection, diagnostics,
    entities::{migration, project},
    error::{database, integrity},
    migrations::Migrator,
    schema,
};
use agentchat_core::Result;
use chrono::{SecondsFormat, Utc};
use sea_orm::{
    ActiveModelTrait, DatabaseConnection, DatabaseTransaction, EntityTrait, IntoActiveModel, Set,
};
use sea_orm_migration::MigratorTrait;
use std::path::Path;
use uuid::Uuid;

pub(crate) async fn apply(
    db: &DatabaseConnection,
    reader: &DatabaseConnection,
    tx: &DatabaseTransaction,
    path: &Path,
    identity: &agentchat_core::project::ProjectIdentity,
    previous: i32,
    progress: &dyn OpenProgress,
) -> Result<()> {
    if connection::preflight(tx, identity).await? != Some(previous) {
        return Err(integrity(
            "Schema changed before the upgrade lock was acquired.",
        ));
    }
    let source = Store {
        db: db.clone(),
        reader: reader.clone(),
        identity: identity.clone(),
    };
    let backup = source
        .backup(path.with_extension(format!(
            "schema-v{previous}-backup-{}.sqlite3",
            Uuid::new_v4()
        )))
        .await?;
    progress.reached(OpenStage::BackupPublished {
        schema_version: previous,
        destination: backup.destination,
    });
    Migrator::up(tx, None).await.map_err(database)?;
    progress.reached(OpenStage::SchemaApplied);
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    for version in previous + 1..=schema::VERSION {
        migration::ActiveModel {
            version: Set(version),
            checksum: Set(schema::checksum(version)),
            applied_at: Set(now.clone()),
        }
        .insert(tx)
        .await
        .map_err(database)?;
    }
    progress.reached(OpenStage::ChecksumsWritten);
    let mut meta = project::Entity::find_by_id(1)
        .one(tx)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Project metadata is missing."))?
        .into_active_model();
    meta.schema_version = Set(schema::VERSION);
    meta.update(tx).await.map_err(database)?;
    progress.reached(OpenStage::MetadataWritten);
    schema::validate(tx).await?;
    diagnostics::check(tx).await?;
    progress.reached(OpenStage::UpgradeValidated);
    Ok(())
}
