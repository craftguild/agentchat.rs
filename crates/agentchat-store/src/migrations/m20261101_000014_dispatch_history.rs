//! Fresh-only Dispatch ownership and append-only delivery transition history.
use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A missing project row alone cannot authorize converting retained facts.
        for table in [
            "project_meta",
            "messages",
            "events",
            "deliveries",
            "launches",
        ] {
            let present = Query::select()
                .expr(1)
                .from(Alias::new(table))
                .limit(1)
                .to_owned();
            if manager
                .get_connection()
                .query_one(&present)
                .await?
                .is_some()
            {
                return Err(DbErr::Custom(
                    "Unsupported old project format; run agentchat install --new-project explicitly."
                        .into(),
                ));
            }
        }
        for table in [
            "message_streams",
            "delivery_timeout_notices",
            "read_confirmations",
            "read_receipts",
            "mentions",
            "deliveries",
            "events",
            "messages",
        ] {
            manager
                .drop_table(Table::drop().table(Alias::new(table)).to_owned())
                .await?;
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("project_meta"))
                    .add_column(
                        ColumnDef::new(Alias::new("next_sequence"))
                            .big_integer()
                            .not_null()
                            .default(1)
                            .check(Expr::col(Alias::new("next_sequence")).gte(1)),
                    )
                    .to_owned(),
            )
            .await?;
        super::dispatch_tables::create(manager).await?;
        super::dispatch_history_tables::create(manager).await?;
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
