//! Immutable second migration, deliberately independent of current entities.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut table = Table::create();
        table
            .table(Alias::new("read_confirmations"))
            .col(text("message_id"))
            .col(text("recipient_id"))
            .col(text("author_id"))
            .col(text("deadline"))
            .col(text("state"))
            .col(text_null("notification_event_id"))
            .primary_key(
                Index::create()
                    .col(Alias::new("message_id"))
                    .col(Alias::new("recipient_id")),
            )
            .check(Expr::col(Alias::new("state")).is_in([
                "waiting",
                "satisfied",
                "timed_out",
                "cancelled",
            ]))
            .check(
                Expr::col(Alias::new("state"))
                    .eq("timed_out")
                    .eq(Expr::col(Alias::new("notification_event_id")).is_not_null()),
            );
        for (column, target, key) in [
            ("message_id", "messages", "id"),
            ("recipient_id", "agents", "id"),
            ("author_id", "agents", "id"),
            ("notification_event_id", "events", "event_id"),
        ] {
            table.foreign_key(
                ForeignKey::create()
                    .from(Alias::new("read_confirmations"), Alias::new(column))
                    .to(Alias::new(target), Alias::new(key))
                    .on_delete(ForeignKeyAction::Restrict),
            );
        }
        manager.create_table(table).await?;
        for (name, columns) in [
            (
                "idx_read_confirmation_due",
                vec!["state", "deadline", "message_id", "recipient_id"],
            ),
            ("idx_read_confirmation_author", vec!["author_id", "state"]),
        ] {
            let mut index = Index::create();
            index.name(name).table(Alias::new("read_confirmations"));
            for column in columns {
                index.col(Alias::new(column));
            }
            manager.create_index(index).await?;
        }
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
