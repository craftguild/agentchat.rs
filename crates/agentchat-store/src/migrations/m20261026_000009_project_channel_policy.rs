//! Add normalized ordered project default-channel policy storage.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_default_channels"))
                    .col(
                        ColumnDef::new(Alias::new("position"))
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("channel_id"))
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project-default-channel")
                            .from(
                                Alias::new("project_default_channels"),
                                Alias::new("channel_id"),
                            )
                            .to(Alias::new("channels"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;
        // Empty means legacy-unset. Core performs the one-time membership
        // cutover together with first policy materialization so readers never
        // observe a partially applied default/channel-membership transition.
        Ok(())
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
