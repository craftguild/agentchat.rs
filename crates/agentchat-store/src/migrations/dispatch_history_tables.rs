//! Recipient lifecycle/history and retained native cursor schema for migration 14.
use super::dispatch_tables::{fk, index};
use sea_orm_migration::{prelude::*, schema::*};

pub(super) async fn create(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let mut delivery = Table::create();
    delivery
        .table(Alias::new("deliveries"))
        .col(text("id").primary_key())
        .col(text("dispatch_id"))
        .col(text("recipient_id"))
        .col(text("state"))
        .col(big_integer("state_version"))
        .col(boolean("wake_eligible"))
        .col(text_null("wake_batch_id"))
        .col(integer("attempts"))
        .col(text("queued_at"));
    for name in [
        "processing_started_at",
        "deadline_at",
        "timed_out_at",
        "delivered_at",
        "left_at",
    ] {
        delivery.col(text_null(name));
    }
    delivery
        .check(Expr::col(Alias::new("attempts")).gte(0))
        .check(Expr::col(Alias::new("state_version")).between(1, 4))
        .check(history_constraint());
    for (column, target, key) in [
        ("dispatch_id", "dispatches", "id"),
        ("recipient_id", "actors", "id"),
        ("wake_batch_id", "wake_batches", "id"),
    ] {
        fk(&mut delivery, "deliveries", column, target, key);
    }
    manager.create_table(delivery.to_owned()).await?;
    for (name, columns, unique) in [
        (
            "uq_delivery_recipient",
            &["dispatch_id", "recipient_id"][..],
            true,
        ),
        (
            "idx_delivery_inbox",
            &["recipient_id", "state", "dispatch_id"][..],
            false,
        ),
        (
            "idx_delivery_deadline",
            &["state", "deadline_at", "id"][..],
            false,
        ),
    ] {
        index(manager, "deliveries", name, columns, unique).await?;
    }

    let mut history = Table::create();
    history
        .table(Alias::new("delivery_state_transitions"))
        .col(text("id").primary_key())
        .col(text("delivery_id"))
        .col(big_integer("ordinal"))
        .col(text_null("from_state"))
        .col(text("to_state"))
        .col(text("changed_at"))
        .col(text_null("deadline_at"))
        .check(Expr::col(Alias::new("ordinal")).between(1, 4));
    let mut allowed = Condition::any().add(
        Condition::all()
            .add(Expr::col(Alias::new("ordinal")).eq(1))
            .add(Expr::col(Alias::new("from_state")).is_null())
            .add(Expr::col(Alias::new("to_state")).eq("pending")),
    );
    for (from, to) in [
        ("pending", "processing"),
        ("pending", "failed_left"),
        ("processing", "delivered"),
        ("processing", "failed_timeout"),
        ("processing", "failed_left"),
        ("failed_timeout", "delivered_late"),
    ] {
        allowed = allowed.add(
            Condition::all()
                .add(Expr::col(Alias::new("ordinal")).gt(1))
                .add(Expr::col(Alias::new("from_state")).eq(from))
                .add(Expr::col(Alias::new("to_state")).eq(to)),
        );
    }
    history.check(allowed).check(
        Condition::any()
            .add(
                Condition::all()
                    .add(Expr::col(Alias::new("to_state")).eq("processing"))
                    .add(Expr::col(Alias::new("deadline_at")).is_not_null())
                    .add(
                        Expr::col(Alias::new("deadline_at"))
                            .gt(Expr::col(Alias::new("changed_at"))),
                    ),
            )
            .add(
                Condition::all()
                    .add(Expr::col(Alias::new("to_state")).ne("processing"))
                    .add(Expr::col(Alias::new("deadline_at")).is_null()),
            ),
    );
    fk(
        &mut history,
        "delivery_state_transitions",
        "delivery_id",
        "deliveries",
        "id",
    );
    manager.create_table(history.to_owned()).await?;
    index(
        manager,
        "delivery_state_transitions",
        "uq_delivery_transition_ordinal",
        &["delivery_id", "ordinal"],
        true,
    )
    .await?;
    streams(manager).await
}

async fn streams(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let mut table = Table::create();
    table
        .table(Alias::new("message_streams"))
        .col(text("generation").primary_key())
        .col(big_integer("settled_seq"))
        .col(text_null("head_delivery"))
        .col(big_integer_null("head_seq"))
        .col(text_null("incarnation"))
        .col(text_null("token"))
        .col(integer_null("attempts"))
        .col(text_null("retry_at"))
        .col(text_null("failure_code"))
        .col(boolean("awaiting_acceptance").default(false))
        .col(boolean("submission_pending").default(false))
        .col(boolean("cancelled").default(false))
        .col(boolean_null("auto_read"))
        .col(integer_null("frame_revision"))
        .check(Expr::col(Alias::new("settled_seq")).gte(0));
    let mut empty = Condition::all();
    let mut head = Condition::all();
    for column in [
        "head_delivery",
        "head_seq",
        "incarnation",
        "token",
        "attempts",
        "retry_at",
        "auto_read",
        "frame_revision",
    ] {
        empty = empty.add(Expr::col(Alias::new(column)).is_null());
        head = head.add(Expr::col(Alias::new(column)).is_not_null());
    }
    table.check(
        Condition::any()
            .add(
                empty
                    .add(Expr::col(Alias::new("failure_code")).is_null())
                    .add(Expr::col(Alias::new("awaiting_acceptance")).eq(false))
                    .add(Expr::col(Alias::new("submission_pending")).eq(false)),
            )
            .add(
                head.add(Expr::col(Alias::new("attempts")).between(1, 3))
                    .add(
                        Expr::col(Alias::new("head_seq")).gt(Expr::col(Alias::new("settled_seq"))),
                    ),
            ),
    );
    for (column, target, key) in [
        ("generation", "launches", "generation"),
        ("head_seq", "dispatches", "creation_seq"),
        ("head_delivery", "deliveries", "id"),
    ] {
        fk(&mut table, "message_streams", column, target, key);
    }
    manager.create_table(table.to_owned()).await
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
