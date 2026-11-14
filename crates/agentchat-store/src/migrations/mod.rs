//! SeaORM 2.x ordered migration registry; normal access uses `Store::open` guards.

mod dispatch_history_tables;
mod dispatch_tables;
pub(crate) mod initial_entities;
mod m20261015_000001_initial;
mod m20261015_000002_read_confirmations;
mod m20261015_000003_history_indexes;
mod m20261018_000004_launches;
mod m20261018_000005_message_streams;
mod m20261018_000006_message_acceptance;
mod m20261019_000007_native_submission;
mod m20261019_000008_claude_uncertainty;
mod m20261026_000009_project_channel_policy;
mod m20261028_000010_project_receipt_settings;
mod m20261028_000011_common_actors;
mod m20261028_000012_receipt_policy;
mod m20261031_000013_delivery_lifecycle;
mod m20261101_000014_dispatch_history;
pub(crate) mod v1_tables;

use sea_orm_migration::prelude::*;

/// Standard SeaORM migration runner for the isolated project database.
///
/// Application opening checks compatibility read-only and holds its OS migration
/// lock before invoking this runner within an immediate transaction. Destructive
/// framework reset/fresh helpers are not exposed by any ac product operation.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261015_000001_initial::Migration),
            Box::new(m20261015_000002_read_confirmations::Migration),
            Box::new(m20261015_000003_history_indexes::Migration),
            Box::new(m20261018_000004_launches::Migration),
            Box::new(m20261018_000005_message_streams::Migration),
            Box::new(m20261018_000006_message_acceptance::Migration),
            Box::new(m20261019_000007_native_submission::Migration),
            Box::new(m20261019_000008_claude_uncertainty::Migration),
            Box::new(m20261026_000009_project_channel_policy::Migration),
            Box::new(m20261028_000010_project_receipt_settings::Migration),
            Box::new(m20261028_000011_common_actors::Migration),
            Box::new(m20261028_000012_receipt_policy::Migration),
            Box::new(m20261031_000013_delivery_lifecycle::Migration),
            Box::new(m20261101_000014_dispatch_history::Migration),
        ]
    }
}

pub(crate) const INITIAL_NAME: &str = "m20261015_000001_initial";
pub(crate) const READ_CONFIRMATIONS_NAME: &str = "m20261015_000002_read_confirmations";
pub(crate) const HISTORY_INDEXES_NAME: &str = "m20261015_000003_history_indexes";
pub(crate) const LAUNCHES_NAME: &str = "m20261018_000004_launches";
pub(crate) const MESSAGE_STREAMS_NAME: &str = "m20261018_000005_message_streams";
pub(crate) const MESSAGE_ACCEPTANCE_NAME: &str = "m20261018_000006_message_acceptance";
pub(crate) const NATIVE_SUBMISSION_NAME: &str = "m20261019_000007_native_submission";
pub(crate) const CLAUDE_UNCERTAINTY_NAME: &str = "m20261019_000008_claude_uncertainty";
pub(crate) const PROJECT_CHANNEL_POLICY_NAME: &str = "m20261026_000009_project_channel_policy";
pub(crate) const PROJECT_RECEIPT_SETTINGS_NAME: &str = "m20261028_000010_project_receipt_settings";
pub(crate) const COMMON_ACTORS_NAME: &str = "m20261028_000011_common_actors";

pub(crate) const RECEIPT_POLICY_NAME: &str = "m20261028_000012_receipt_policy";

pub(crate) const DELIVERY_LIFECYCLE_NAME: &str = "m20261031_000013_delivery_lifecycle";
pub(crate) const DISPATCH_HISTORY_NAME: &str = "m20261101_000014_dispatch_history";
