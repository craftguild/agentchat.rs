//! One body cursor per launch; ACK/read state remains in its original tables.
use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut table = Table::create();
        table
            .table(Alias::new("message_streams"))
            .col(text("generation").primary_key())
            .col(big_integer("accepted_seq"))
            .col(text_null("head_delivery"))
            .col(big_integer_null("head_seq"))
            .col(text_null("incarnation"))
            .col(text_null("token"))
            .col(integer_null("attempts"))
            .col(text_null("retry_at"))
            .col(text_null("failure_code"))
            .col(boolean("cancelled").default(false));
        let mut empty = Condition::all();
        let mut head = Condition::all();
        for column in [
            "head_delivery",
            "head_seq",
            "incarnation",
            "token",
            "attempts",
            "retry_at",
        ] {
            empty = empty.add(Expr::col(Alias::new(column)).is_null());
            head = head.add(Expr::col(Alias::new(column)).is_not_null());
        }
        table.check(
            Condition::any()
                .add(empty.add(Expr::col(Alias::new("failure_code")).is_null()))
                .add(
                    head.add(Expr::col(Alias::new("attempts")).between(1, 3))
                        .add(
                            Expr::col(Alias::new("head_seq"))
                                .gt(Expr::col(Alias::new("accepted_seq"))),
                        ),
                ),
        );
        for (column, target, key) in [
            ("generation", "launches", "generation"),
            ("accepted_seq", "events", "seq"),
            ("head_seq", "events", "seq"),
            ("head_delivery", "deliveries", "id"),
        ] {
            table.foreign_key(
                ForeignKey::create()
                    .from(Alias::new("message_streams"), Alias::new(column))
                    .to(Alias::new(target), Alias::new(key))
                    .on_delete(ForeignKeyAction::Restrict),
            );
        }
        manager.create_table(table).await?;
        // Existing v4 launches retain their exact boundary and terminal status.
        // The migration grants no native authority and cannot restore backlog
        // cancelled by an earlier retirement. No evolving-model sync is used.
        manager
            .get_connection()
            .execute(
                &Query::insert()
                    .into_table(Alias::new("message_streams"))
                    .columns([
                        Alias::new("generation"),
                        Alias::new("accepted_seq"),
                        Alias::new("cancelled"),
                    ])
                    .select_from(
                        Query::select()
                            .columns([Alias::new("generation"), Alias::new("start_seq")])
                            .expr(Expr::col(Alias::new("retired_at")).is_not_null())
                            .from(Alias::new("launches"))
                            .to_owned(),
                    )
                    .map_err(|_| {
                        DbErr::Custom("Invalid message stream migration selection.".into())
                    })?
                    .to_owned(),
            )
            .await?;
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
