//! Immutable lookup indexes for safety checks over indefinitely retained history.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Every commit checks for unpublished events, including transactions
        // which allocate none. Index the predicate without weakening the guard.
        manager
            .create_index(
                Index::create()
                    .name("idx_event_sealed")
                    .table(Alias::new("events"))
                    .col(Alias::new("sealed"))
                    .to_owned(),
            )
            .await?;
        // Unlike the existing partial uniqueness constraint, this full index
        // supports parameterized state lookup and retirement of old incarnations.
        manager
            .create_index(
                Index::create()
                    .name("idx_batch_recipient_state_incarnation")
                    .table(Alias::new("wake_batches"))
                    .col(Alias::new("recipient_id"))
                    .col(Alias::new("state"))
                    .col(Alias::new("incarnation"))
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
