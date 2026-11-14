//! Versioned schema builders and transactional bootstrap, with no SQL literals.

pub(crate) mod business;
pub(crate) mod indexes;

use agentchat_core::{Result, project::ProjectIdentity};
use chrono::{SecondsFormat, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseTransaction, EntityTrait, QueryOrder, QuerySelect,
    Set,
    sea_query::{Alias, Query},
};
use sea_orm_migration::{MigrationStatus, MigratorTrait};
use sha2::{Digest, Sha256};

use crate::{
    entities::{migration, project},
    error::{database, integrity},
    migrations::{
        CLAUDE_UNCERTAINTY_NAME, COMMON_ACTORS_NAME, DELIVERY_LIFECYCLE_NAME,
        DISPATCH_HISTORY_NAME, HISTORY_INDEXES_NAME, INITIAL_NAME, LAUNCHES_NAME,
        MESSAGE_ACCEPTANCE_NAME, MESSAGE_STREAMS_NAME, Migrator, NATIVE_SUBMISSION_NAME,
        PROJECT_CHANNEL_POLICY_NAME, PROJECT_RECEIPT_SETTINGS_NAME, READ_CONFIRMATIONS_NAME,
        RECEIPT_POLICY_NAME,
    },
};

pub(crate) const VERSION: i32 = 14;
pub(crate) const FORMAT: i32 = 3;

pub(crate) fn checksum(version: i32) -> String {
    // Once released, these migration sources are immutable. A code/schema drift
    // must be a new migration rather than silently changing a retained database.
    let source = concat!(
        include_str!("migrations/m20261015_000001_initial.rs"),
        include_str!("migrations/v1_tables.rs"),
        include_str!("schema/business.rs"),
        include_str!("schema/indexes.rs")
    );
    let mut hash = Sha256::new();
    if version == 1 {
        hash.update(source.as_bytes());
        hash.update(crate::migrations::v1_tables::MODEL_SOURCE.as_bytes());
    } else if version == 2 {
        hash.update(include_str!(
            "migrations/m20261015_000002_read_confirmations.rs"
        ));
    } else if version == 3 {
        hash.update(include_str!(
            "migrations/m20261015_000003_history_indexes.rs"
        ));
    } else if version == 4 {
        hash.update(include_str!("migrations/m20261018_000004_launches.rs"));
    } else if version == 5 {
        hash.update(include_str!(
            "migrations/m20261018_000005_message_streams.rs"
        ));
    } else if version == 6 {
        hash.update(include_str!(
            "migrations/m20261018_000006_message_acceptance.rs"
        ));
    } else if version == 7 {
        hash.update(include_str!(
            "migrations/m20261019_000007_native_submission.rs"
        ));
    } else if version == 8 {
        hash.update(include_str!(
            "migrations/m20261019_000008_claude_uncertainty.rs"
        ));
    } else if version == 9 {
        hash.update(include_str!(
            "migrations/m20261026_000009_project_channel_policy.rs"
        ));
    } else if version == 10 {
        hash.update(include_str!(
            "migrations/m20261028_000010_project_receipt_settings.rs"
        ));
    } else if version == 11 {
        hash.update(include_str!("migrations/m20261028_000011_common_actors.rs"));
    } else if version == 12 {
        hash.update(include_str!(
            "migrations/m20261028_000012_receipt_policy.rs"
        ));
    } else if version == 13 {
        hash.update(include_str!(
            "migrations/m20261031_000013_delivery_lifecycle.rs"
        ));
    } else {
        assert_eq!(version, 14, "checksum requires a registered migration");
        hash.update(include_str!(
            "migrations/m20261101_000014_dispatch_history.rs"
        ));
        hash.update(include_str!("migrations/dispatch_tables.rs"));
        hash.update(include_str!("migrations/dispatch_history_tables.rs"));
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) async fn bootstrap(
    tx: &DatabaseTransaction,
    identity: &ProjectIdentity,
    version: i32,
) -> Result<()> {
    // The outer immediate transaction covers framework tracking, schema and
    // project seed/checksum rows. The migration also opts into a savepoint.
    Migrator::up(tx, Some(version as u32))
        .await
        .map_err(database)?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    // Historical schema fixtures have no allocator column. Seed their original
    // metadata shape explicitly rather than querying them with evolving models.
    tx.execute(
        &Query::insert()
            .into_table(Alias::new("project_meta"))
            .columns(
                [
                    "singleton",
                    "project_id",
                    "scope",
                    "created_at",
                    "schema_version",
                    "format_version",
                ]
                .map(Alias::new),
            )
            .values_panic([
                1.into(),
                identity.id.to_string().into(),
                identity.scope.as_str().into(),
                now.clone().into(),
                version.into(),
                (if version >= 14 {
                    FORMAT
                } else if version == 13 {
                    2
                } else {
                    1
                })
                .into(),
            ])
            .to_owned(),
    )
    .await
    .map_err(database)?;
    if version >= 10 {
        crate::receipt_settings::save(tx, &Default::default()).await?;
    }
    for version in 1..=version {
        migration::ActiveModel {
            version: Set(version),
            checksum: Set(checksum(version)),
            applied_at: Set(now.clone()),
        }
        .insert(tx)
        .await
        .map_err(database)?;
    }
    Ok(())
}

pub(crate) async fn validate<C: ConnectionTrait>(db: &C) -> Result<()> {
    let version = project::Entity::find_by_id(1)
        .one(db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Project metadata is missing."))?
        .schema_version;
    validate_version(db, version).await
}

pub(crate) async fn validate_version<C: ConnectionTrait>(db: &C, version: i32) -> Result<()> {
    let status = Migrator::get_migration_with_status_read_only(db)
        .await
        .map_err(|error| match error {
            sea_orm::DbErr::Custom(_) => agentchat_core::Error::new(
                agentchat_core::ErrorCode::UnsupportedSchema,
                "Database contains migrations outside the supported registry.",
            ),
            error => database(error),
        })?;
    if !(1..=VERSION).contains(&version)
        || status.len() != VERSION as usize
        || status
            .iter()
            .zip([
                INITIAL_NAME,
                READ_CONFIRMATIONS_NAME,
                HISTORY_INDEXES_NAME,
                LAUNCHES_NAME,
                MESSAGE_STREAMS_NAME,
                MESSAGE_ACCEPTANCE_NAME,
                NATIVE_SUBMISSION_NAME,
                CLAUDE_UNCERTAINTY_NAME,
                PROJECT_CHANNEL_POLICY_NAME,
                PROJECT_RECEIPT_SETTINGS_NAME,
                COMMON_ACTORS_NAME,
                RECEIPT_POLICY_NAME,
                DELIVERY_LIFECYCLE_NAME,
                DISPATCH_HISTORY_NAME,
            ])
            .enumerate()
            .any(|(i, (s, name))| {
                s.name() != name
                    || s.status()
                        != if i < version as usize {
                            MigrationStatus::Applied
                        } else {
                            MigrationStatus::Pending
                        }
            })
    {
        return Err(integrity(
            "SeaORM migration tracking is missing or incomplete.",
        ));
    }
    let rows = migration::Entity::find()
        .order_by_asc(migration::Column::Version)
        .limit(VERSION as u64 + 1)
        .all(db)
        .await
        .map_err(database)?;
    if rows.iter().any(|row| row.version > VERSION) {
        return Err(agentchat_core::Error::new(
            agentchat_core::ErrorCode::UnsupportedSchema,
            "Database migration ledger is newer than supported.",
        ));
    }
    if rows.len() != version as usize
        || rows
            .iter()
            .enumerate()
            .any(|(i, row)| row.version != i as i32 + 1 || row.checksum != checksum(row.version))
    {
        return Err(integrity("Missing or mismatched migration checksum."));
    }
    Ok(())
}
