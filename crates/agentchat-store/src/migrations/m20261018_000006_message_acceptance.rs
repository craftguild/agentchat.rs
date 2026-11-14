//! Pending local publication is distinct from native success or a failed attempt.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("message_streams"))
                    .add_column(
                        ColumnDef::new(Alias::new("awaiting_acceptance"))
                            .boolean()
                            .not_null()
                            .default(false)
                            .check(
                                Condition::any()
                                    .add(Expr::col(Alias::new("awaiting_acceptance")).eq(false))
                                    .add(
                                        Condition::all()
                                            .add(
                                                Expr::col(Alias::new("head_delivery"))
                                                    .is_not_null(),
                                            )
                                            .add(Expr::col(Alias::new("failure_code")).is_null()),
                                    ),
                            ),
                    )
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
