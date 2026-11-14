//! Initial relational schema, applied and tracked by the SeaORM 2.x migrator.

use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm_migration::prelude::*;

use super::v1_tables;
use crate::schema::{business, indexes};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut metadata = v1_tables::table("project_meta")?;
        metadata.check(Expr::col(Alias::new("singleton")).eq(1));
        metadata.check(Expr::col(Alias::new("scope")).is_in(["repository", "directory"]));
        manager.create_table(metadata).await?;
        manager
            .create_table(v1_tables::table("schema_migrations")?)
            .await?;
        business::create(manager).await?;
        indexes::create(manager).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Retained history is part of the v1 contract. A normal project open
        // never downgrades, and an explicit framework down must not erase it.
        Err(DbErr::Custom(
            "Agent Chat does not support schema downgrade.".into(),
        ))
    }

    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }
}
