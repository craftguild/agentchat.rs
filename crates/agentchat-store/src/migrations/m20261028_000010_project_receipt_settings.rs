//! Add receipt policy with conservative legacy defaults, without backfilling facts.

use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table = Alias::new("project_receipt_settings");
        manager
            .create_table(
                Table::create()
                    .table(table.clone())
                    .col(
                        ColumnDef::new(Alias::new("singleton"))
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("auto_ack")).boolean().not_null())
                    .col(ColumnDef::new(Alias::new("auto_read")).boolean().not_null())
                    .col(
                        ColumnDef::new(Alias::new("delivery_confirmation_timeout_seconds"))
                            .integer()
                            .not_null(),
                    )
                    .check(Expr::col(Alias::new("singleton")).eq(1))
                    .check(Expr::col(Alias::new("auto_ack")).is_in([false, true]))
                    .check(Expr::col(Alias::new("auto_read")).is_in([false, true]))
                    .check(
                        Expr::col(Alias::new("delivery_confirmation_timeout_seconds"))
                            .between(1, 2_592_000),
                    )
                    .to_owned(),
            )
            .await?;
        // Only a brand-new namespace bootstrap changes auto_read to true.
        // Every upgrade keeps the old explicit-read-only interpretation.
        manager
            .get_connection()
            .execute(
                &Query::insert()
                    .into_table(table)
                    .columns([
                        Alias::new("singleton"),
                        Alias::new("auto_ack"),
                        Alias::new("auto_read"),
                        Alias::new("delivery_confirmation_timeout_seconds"),
                    ])
                    .values_panic([1.into(), true.into(), false.into(), 60.into()])
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "AgentChat does not support schema downgrade.".into(),
        ))
    }

    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }
}
