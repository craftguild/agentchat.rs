//! Independent native acceptance, first-read kinds and captured delivery deadlines.
use sea_orm::ConnectionTrait;
use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("deliveries"))
                    .add_column(text_null("accepted_at"))
                    .to_owned(),
            )
            .await?;
        // Old ACKs and reads do not prove native arrival. Original retained heads
        // preserve their historical policy and explicit frame revision only.
        for column in [
            boolean_null("auto_ack"),
            boolean_null("auto_read"),
            integer_null("frame_revision"),
            text_null("accepted_at"),
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("message_streams"))
                        .add_column(column)
                        .to_owned(),
                )
                .await?;
        }
        db.execute(
            &Query::update()
                .table(Alias::new("message_streams"))
                .value(Alias::new("auto_ack"), true)
                .value(Alias::new("auto_read"), false)
                .value(Alias::new("frame_revision"), 1)
                .and_where(Expr::col(Alias::new("head_delivery")).is_not_null())
                .to_owned(),
        )
        .await?;
        let table = Alias::new("read_receipts_v12");
        manager
            .create_table(
                Table::create()
                    .table(table.clone())
                    .col(text("message_id"))
                    .col(text("actor_id"))
                    .col(text("kind"))
                    .col(text("first_read_at"))
                    .primary_key(
                        Index::create()
                            .col(Alias::new("message_id"))
                            .col(Alias::new("actor_id"))
                            .col(Alias::new("kind")),
                    )
                    .check(Expr::col(Alias::new("kind")).is_in(["automatic", "explicit"]))
                    .foreign_key(
                        ForeignKey::create()
                            .from(table.clone(), Alias::new("message_id"))
                            .to(Alias::new("messages"), Alias::new("id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(table.clone(), Alias::new("actor_id"))
                            .to(Alias::new("actors"), Alias::new("id")),
                    )
                    .to_owned(),
            )
            .await?;
        db.execute(
            &Query::insert()
                .into_table(table.clone())
                .columns(["message_id", "actor_id", "kind", "first_read_at"].map(Alias::new))
                .select_from(
                    Query::select()
                        .columns(["message_id", "actor_id"].map(Alias::new))
                        .expr(Expr::val("explicit"))
                        .column(Alias::new("first_read_at"))
                        .from(Alias::new("read_receipts"))
                        .to_owned(),
                )
                .map_err(|error| DbErr::Custom(error.to_string()))?
                .to_owned(),
        )
        .await?;
        manager
            .drop_table(Table::drop().table(Alias::new("read_receipts")).to_owned())
            .await?;
        manager
            .rename_table(
                Table::rename()
                    .table(table, Alias::new("read_receipts"))
                    .to_owned(),
            )
            .await?;
        let watches = Alias::new("confirmation_watches");
        manager
            .create_table(
                Table::create()
                    .table(watches.clone())
                    .col(text("message_id").primary_key())
                    .col(text("author_id"))
                    .col(text("deadline"))
                    .col(text("state"))
                    .col(text_null("notification_event_id"))
                    .check(Expr::col(Alias::new("state")).is_in([
                        "waiting",
                        "confirmed",
                        "timed_out",
                        "cancelled",
                    ]))
                    .check(
                        Cond::any()
                            .add(
                                Cond::all()
                                    .add(Expr::col(Alias::new("state")).eq("timed_out"))
                                    .add(
                                        Expr::col(Alias::new("notification_event_id"))
                                            .is_not_null(),
                                    ),
                            )
                            .add(
                                Cond::all()
                                    .add(Expr::col(Alias::new("state")).ne("timed_out"))
                                    .add(Expr::col(Alias::new("notification_event_id")).is_null()),
                            ),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(watches.clone(), Alias::new("message_id"))
                            .to(Alias::new("messages"), Alias::new("id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(watches.clone(), Alias::new("author_id"))
                            .to(Alias::new("actors"), Alias::new("id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(watches.clone(), Alias::new("notification_event_id"))
                            .to(Alias::new("events"), Alias::new("event_id")),
                    )
                    .to_owned(),
            )
            .await?;
        let targets = Alias::new("confirmation_targets");
        manager
            .create_table(
                Table::create()
                    .table(targets.clone())
                    .col(text("message_id"))
                    .col(text("recipient_id"))
                    .col(text("state"))
                    .primary_key(
                        Index::create()
                            .col(Alias::new("message_id"))
                            .col(Alias::new("recipient_id")),
                    )
                    .check(Expr::col(Alias::new("state")).is_in([
                        "waiting",
                        "confirmed",
                        "timed_out",
                        "cancelled",
                    ]))
                    .foreign_key(
                        ForeignKey::create()
                            .from(targets.clone(), Alias::new("message_id"))
                            .to(watches.clone(), Alias::new("message_id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(targets, Alias::new("recipient_id"))
                            .to(Alias::new("actors"), Alias::new("id")),
                    )
                    .to_owned(),
            )
            .await?;
        for (name, columns) in [
            (
                "idx_confirmation_due",
                vec!["state", "deadline", "message_id"],
            ),
            ("idx_confirmation_author", vec!["author_id", "state"]),
        ] {
            let mut index = Index::create();
            index.name(name).table(watches.clone());
            for col in columns {
                index.col(Alias::new(col));
            }
            manager.create_index(index.to_owned()).await?;
        }
        // Waiting explicit-read deadlines are retired, never converted to new
        // delivery deadlines; terminal watches and their historical events stay.
        db.execute(
            &Query::update()
                .table(Alias::new("read_confirmations"))
                .value(Alias::new("state"), "cancelled")
                .and_where(Expr::col(Alias::new("state")).eq("waiting"))
                .to_owned(),
        )
        .await?;
        let positive_events = Query::select()
            .column(Alias::new("event_id"))
            .from(Alias::new("events"))
            .and_where(Expr::col(Alias::new("kind")).eq("message.read"))
            .to_owned();
        let heads = Query::select()
            .column(Alias::new("head_delivery"))
            .from(Alias::new("message_streams"))
            .and_where(Expr::col(Alias::new("head_delivery")).is_not_null())
            .to_owned();
        // Zero attempts, no assignment and no retained head are all required.
        // In particular, manual ACK cannot disguise an unresolved native head.
        db.execute(
            &Query::update()
                .table(Alias::new("deliveries"))
                .value(Alias::new("state"), "cancelled")
                .and_where(Expr::col(Alias::new("event_id")).in_subquery(positive_events))
                .and_where(Expr::col(Alias::new("state")).is_in(["pending", "inflight"]))
                .and_where(Expr::col(Alias::new("attempts")).eq(0))
                .and_where(Expr::col(Alias::new("wake_batch_id")).is_null())
                .and_where(Expr::col(Alias::new("id")).not_in_subquery(heads))
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
