//! Generated initial schema registry: model-derived columns and primary keys.
//! Later model changes require another named MigrationTrait implementation.

use super::initial_entities;
use sea_orm::{DbBackend, Schema, sea_query::TableCreateStatement};
use sea_orm_migration::DbErr;

pub(crate) fn table(name: &str) -> Result<TableCreateStatement, DbErr> {
    let schema = Schema::new(DbBackend::Sqlite);
    let table = match name {
        "agents" => schema.create_table_from_entity(initial_entities::agent::Entity),
        "channels" => schema.create_table_from_entity(initial_entities::channel::Entity),
        "deliveries" => schema.create_table_from_entity(initial_entities::delivery::Entity),
        "events" => schema.create_table_from_entity(initial_entities::event::Entity),
        "idempotency" => schema.create_table_from_entity(initial_entities::idempotency::Entity),
        "channel_memberships" => {
            schema.create_table_from_entity(initial_entities::membership::Entity)
        }
        "mentions" => schema.create_table_from_entity(initial_entities::mention::Entity),
        "messages" => schema.create_table_from_entity(initial_entities::message::Entity),
        "schema_migrations" => schema.create_table_from_entity(initial_entities::migration::Entity),
        "project_meta" => schema.create_table_from_entity(initial_entities::project::Entity),
        "read_receipts" => schema.create_table_from_entity(initial_entities::receipt::Entity),
        "reservations" => schema.create_table_from_entity(initial_entities::reservation::Entity),
        "reservation_patterns" => {
            schema.create_table_from_entity(initial_entities::reservation_pattern::Entity)
        }
        "sessions" => schema.create_table_from_entity(initial_entities::session::Entity),
        "wake_batches" => schema.create_table_from_entity(initial_entities::wake_batch::Entity),
        _ => return Err(DbErr::Custom("Unknown initial model table.".into())),
    };
    Ok(table)
}

pub(crate) const MODEL_SOURCE: &str = concat!(
    include_str!("initial_entities/agent.rs"),
    include_str!("initial_entities/channel.rs"),
    include_str!("initial_entities/delivery.rs"),
    include_str!("initial_entities/event.rs"),
    include_str!("initial_entities/idempotency.rs"),
    include_str!("initial_entities/membership.rs"),
    include_str!("initial_entities/mention.rs"),
    include_str!("initial_entities/message.rs"),
    include_str!("initial_entities/migration.rs"),
    include_str!("initial_entities/project.rs"),
    include_str!("initial_entities/receipt.rs"),
    include_str!("initial_entities/reservation.rs"),
    include_str!("initial_entities/reservation_pattern.rs"),
    include_str!("initial_entities/session.rs"),
    include_str!("initial_entities/wake_batch.rs"),
);

// Source artifacts stay below the ordinary production code-line limit.
