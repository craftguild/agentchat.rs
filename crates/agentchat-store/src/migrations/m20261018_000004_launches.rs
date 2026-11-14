//! Durable generations extend, rather than synchronize, the frozen baseline.
use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut table = Table::create();
        table
            .table(Alias::new("launches"))
            .col(text("generation").primary_key())
            .col(text("agent_id"))
            .col(text("context_id").unique_key())
            .col(text("incarnation").unique_key())
            .col(text("owner"))
            .col(big_integer("start_seq"))
            .col(text("started_at"))
            .col(text_null("retired_at"))
            .col(text_null("retirement_reason"))
            .check(
                Expr::col(Alias::new("retired_at"))
                    .is_null()
                    .eq(Expr::col(Alias::new("retirement_reason")).is_null()),
            );
        for (column, target, key) in [("agent_id", "agents", "id"), ("start_seq", "events", "seq")]
        {
            table.foreign_key(
                ForeignKey::create()
                    .from(Alias::new("launches"), Alias::new(column))
                    .to(Alias::new(target), Alias::new(key))
                    .on_delete(ForeignKeyAction::Restrict),
            );
        }
        manager.create_table(table).await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_launch_agent_sequence")
                    .table(Alias::new("launches"))
                    .col(Alias::new("agent_id"))
                    .col(Alias::new("start_seq"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_launch_active_agent")
                    .table(Alias::new("launches"))
                    .col(Alias::new("agent_id"))
                    .unique()
                    .and_where(Expr::col(Alias::new("retired_at")).is_null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_launch_active_generation")
                    .table(Alias::new("launches"))
                    .col(Alias::new("retired_at"))
                    .col(Alias::new("generation"))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Agent Chat does not support schema downgrade.".into(),
        ))
    }
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }
}
