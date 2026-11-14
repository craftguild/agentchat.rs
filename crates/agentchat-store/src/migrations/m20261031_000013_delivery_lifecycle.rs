//! Fresh-format delivery history; released migrations remain immutable.
use sea_orm::ConnectionTrait;
use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // This registry is also used to bootstrap a new empty namespace. Existing
        // history is never converted: the normal read-only open gate rejects it,
        // and this guard prevents direct migrator use from bypassing that gate.
        let old = Query::select()
            .column(Alias::new("project_id"))
            .from(Alias::new("project_meta"))
            .limit(1)
            .to_owned();
        if manager.get_connection().query_one(&old).await?.is_some() {
            return Err(DbErr::Custom(
                "Unsupported old project format; run agentchat install --new-project explicitly."
                    .into(),
            ));
        }
        // A single per-actor forward boundary preserves the existing explicit
        // author-Leave cancellation policy without another per-message watch.
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("actors"))
                    .add_column(
                        ColumnDef::new(Alias::new("timeout_notice_after_seq"))
                            .big_integer()
                            .not_null()
                            .default(0)
                            .check(Expr::col(Alias::new("timeout_notice_after_seq")).gte(0)),
                    )
                    .to_owned(),
            )
            .await?;
        rebuild_batches(manager).await?;
        manager
            .drop_table(Table::drop().table(Alias::new("deliveries")).to_owned())
            .await?;
        let mut table = Table::create();
        table
            .table(Alias::new("deliveries"))
            .col(text("id").primary_key())
            .col(text("event_id"))
            .col(text("recipient_id"))
            .col(text("state"))
            .col(boolean("wake_eligible"))
            .col(text_null("wake_batch_id"))
            .col(integer("attempts"))
            .col(text("queued_at"));
        for column in [
            "processing_started_at",
            "deadline_at",
            "timed_out_at",
            "delivered_at",
            "left_at",
        ] {
            table.col(text_null(column));
        }
        for (column, target, key) in [
            ("event_id", "events", "event_id"),
            ("recipient_id", "actors", "id"),
            ("wake_batch_id", "wake_batches", "id"),
        ] {
            table.foreign_key(
                ForeignKey::create()
                    .from(Alias::new("deliveries"), Alias::new(column))
                    .to(Alias::new(target), Alias::new(key)),
            );
        }
        table.check(Expr::col(Alias::new("attempts")).gte(0));
        table.check(history_constraint());
        manager.create_table(table.to_owned()).await?;
        for (name, columns, unique) in [
            (
                "uq_delivery_recipient",
                vec!["event_id", "recipient_id"],
                true,
            ),
            (
                "idx_delivery_inbox",
                vec!["recipient_id", "state", "event_id"],
                false,
            ),
            (
                "idx_delivery_deadline",
                vec!["state", "deadline_at", "id"],
                false,
            ),
        ] {
            let mut index = Index::create();
            index.name(name).table(Alias::new("deliveries"));
            for column in columns {
                index.col(Alias::new(column));
            }
            if unique {
                index.unique();
            }
            manager.create_index(index.to_owned()).await?;
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("message_streams"))
                    .rename_column(Alias::new("accepted_seq"), Alias::new("settled_seq"))
                    .to_owned(),
            )
            .await?;
        for column in ["auto_ack", "accepted_at"] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("message_streams"))
                        .drop_column(Alias::new(column))
                        .to_owned(),
                )
                .await?;
        }
        for table in [
            "confirmation_targets",
            "confirmation_watches",
            "project_receipt_settings",
        ] {
            manager
                .drop_table(Table::drop().table(Alias::new(table)).to_owned())
                .await?;
        }
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_receipt_settings"))
                    .col(integer("singleton").primary_key())
                    .col(boolean("auto_read"))
                    .col(integer("delivery_confirmation_timeout_seconds"))
                    .check(Expr::col(Alias::new("singleton")).eq(1))
                    .check(Expr::col(Alias::new("auto_read")).is_in([false, true]))
                    .check(
                        Expr::col(Alias::new("delivery_confirmation_timeout_seconds"))
                            .between(1, 2_592_000),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute(
                &Query::insert()
                    .into_table(Alias::new("project_receipt_settings"))
                    .columns([
                        Alias::new("singleton"),
                        Alias::new("auto_read"),
                        Alias::new("delivery_confirmation_timeout_seconds"),
                    ])
                    .values_panic([1.into(), true.into(), 3_600.into()])
                    .to_owned(),
            )
            .await?;
        // Only a coalescing identity remains. Recipient outcomes and deadlines
        // belong exclusively to their original delivery rows.
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("delivery_timeout_notices"))
                    .col(text("message_id").primary_key())
                    .col(text("event_id"))
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("delivery_timeout_notices"),
                                Alias::new("message_id"),
                            )
                            .to(Alias::new("messages"), Alias::new("id")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("delivery_timeout_notices"),
                                Alias::new("event_id"),
                            )
                            .to(Alias::new("events"), Alias::new("event_id")),
                    )
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

fn history_constraint() -> Condition {
    let mut states = Condition::any();
    for (state, processing, timeout, delivered, left) in [
        ("pending", false, false, false, false),
        ("processing", true, false, false, false),
        ("delivered", true, false, true, false),
        ("failed_timeout", true, true, false, false),
        ("delivered_late", true, true, true, false),
        ("failed_left", false, false, false, true),
        ("failed_left", true, false, false, true),
    ] {
        let mut row = Condition::all().add(Expr::col(Alias::new("state")).eq(state));
        for (column, present) in [
            ("processing_started_at", processing),
            ("deadline_at", processing),
            ("timed_out_at", timeout),
            ("delivered_at", delivered),
            ("left_at", left),
        ] {
            row = row.add(if present {
                Expr::col(Alias::new(column)).is_not_null()
            } else {
                Expr::col(Alias::new(column)).is_null()
            });
        }
        if processing {
            row = row.add(
                Expr::col(Alias::new("deadline_at"))
                    .gt(Expr::col(Alias::new("processing_started_at"))),
            );
        }
        if timeout {
            row = row.add(
                Expr::col(Alias::new("timed_out_at")).gt(Expr::col(Alias::new("deadline_at"))),
            );
        }
        if delivered {
            row = row.add(if timeout {
                Expr::col(Alias::new("delivered_at")).gt(Expr::col(Alias::new("deadline_at")))
            } else {
                Expr::col(Alias::new("delivered_at")).lte(Expr::col(Alias::new("deadline_at")))
            });
        }
        states = states.add(row);
    }
    states
}

async fn rebuild_batches(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .drop_table(Table::drop().table(Alias::new("wake_batches")).to_owned())
        .await?;
    manager
        .create_table(
            Table::create()
                .table(Alias::new("wake_batches"))
                .col(text("id").primary_key())
                .col(text("recipient_id"))
                .col(text("incarnation"))
                .col(text("state"))
                .col(text_null("dispatch_ref"))
                .col(text_null("turn_ref"))
                .col(integer("attempts"))
                .col(text_null("next_eligible_at"))
                .col(text_null("failure_code"))
                .check(Expr::col(Alias::new("state")).is_in([
                    "pending",
                    "inflight",
                    "stalled",
                    "settled",
                    "cancelled",
                ]))
                .check(Expr::col(Alias::new("attempts")).between(0, 3))
                .foreign_key(
                    ForeignKey::create()
                        .from(Alias::new("wake_batches"), Alias::new("recipient_id"))
                        .to(Alias::new("actors"), Alias::new("id")),
                )
                .foreign_key(
                    ForeignKey::create()
                        .from(Alias::new("wake_batches"), Alias::new("incarnation"))
                        .to(Alias::new("sessions"), Alias::new("incarnation")),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("uq_batch_outstanding")
                .table(Alias::new("wake_batches"))
                .col(Alias::new("recipient_id"))
                .col(Alias::new("incarnation"))
                .unique()
                .and_where(Expr::col(Alias::new("state")).is_in(["pending", "inflight", "stalled"]))
                .to_owned(),
        )
        .await?;
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
        .await?;
    Ok(())
}
