//! Dispatch Header/subtypes and independent audit records for named migration 14.
use sea_orm_migration::{prelude::*, schema::*};

pub(super) fn fk(
    table: &mut TableCreateStatement,
    owner: &str,
    column: &str,
    target: &str,
    key: &str,
) {
    table.foreign_key(
        ForeignKey::create()
            .from(Alias::new(owner), Alias::new(column))
            .to(Alias::new(target), Alias::new(key))
            .on_delete(ForeignKeyAction::Restrict),
    );
}

pub(super) async fn index(
    manager: &SchemaManager<'_>,
    table: &str,
    name: &str,
    columns: &[&str],
    unique: bool,
) -> Result<(), DbErr> {
    let mut value = Index::create();
    value.table(Alias::new(table)).name(name);
    for column in columns {
        value.col(Alias::new(*column));
    }
    if unique {
        value.unique();
    }
    manager.create_index(value.to_owned()).await
}

pub(super) async fn create(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let mut header = Table::create();
    header
        .table(Alias::new("dispatches"))
        .col(text("id").primary_key())
        .col(text("kind"))
        .col(text_null("channel_id"))
        .col(text("created_at"))
        .col(big_integer("creation_seq"))
        .col(boolean("sealed"))
        .check(Expr::col(Alias::new("kind")).is_in(["message", "notice"]))
        .check(Expr::col(Alias::new("creation_seq")).gt(0));
    fk(&mut header, "dispatches", "channel_id", "channels", "id");
    manager.create_table(header.to_owned()).await?;
    index(
        manager,
        "dispatches",
        "uq_dispatch_sequence",
        &["creation_seq"],
        true,
    )
    .await?;
    index(
        manager,
        "dispatches",
        "idx_dispatch_channel_sequence",
        &["channel_id", "creation_seq"],
        false,
    )
    .await?;

    let mut message = Table::create();
    message
        .table(Alias::new("dispatch_messages"))
        .col(text("dispatch_id").primary_key())
        .col(text("author_id"))
        .col(text_null("root_message_id"))
        .col(text("body"))
        .check(
            Expr::col(Alias::new("root_message_id"))
                .is_null()
                .or(Expr::col(Alias::new("root_message_id"))
                    .ne(Expr::col(Alias::new("dispatch_id")))),
        )
        .check(
            Expr::expr(
                Func::cust(Alias::new("length"))
                    .arg(Expr::col(Alias::new("body")).cast_as(Alias::new("blob"))),
            )
            .between(1, 65_536),
        );
    fk(
        &mut message,
        "dispatch_messages",
        "dispatch_id",
        "dispatches",
        "id",
    );
    fk(
        &mut message,
        "dispatch_messages",
        "author_id",
        "actors",
        "id",
    );
    fk(
        &mut message,
        "dispatch_messages",
        "root_message_id",
        "dispatch_messages",
        "dispatch_id",
    );
    manager.create_table(message.to_owned()).await?;
    index(
        manager,
        "dispatch_messages",
        "idx_dispatch_message_root",
        &["root_message_id", "dispatch_id"],
        false,
    )
    .await?;

    let mut notice = Table::create();
    notice
        .table(Alias::new("dispatch_notices"))
        .col(text("dispatch_id").primary_key())
        .col(text("kind"))
        .col(json("data"));
    fk(
        &mut notice,
        "dispatch_notices",
        "dispatch_id",
        "dispatches",
        "id",
    );
    manager.create_table(notice.to_owned()).await?;

    let mut event = Table::create();
    event
        .table(Alias::new("events"))
        .col(integer("ordinal").primary_key().auto_increment())
        .col(big_integer("seq"))
        .col(text("event_id"))
        .col(integer("version"))
        .col(text("kind"))
        .col(text("occurred_at"))
        .col(text_null("actor_id"))
        .col(text_null("channel_id"))
        .col(text_null("message_id"))
        .col(json("data"))
        .col(boolean("sealed"))
        .check(Expr::col(Alias::new("seq")).gt(0))
        .check(Expr::col(Alias::new("version")).eq(1));
    for (column, target, key) in [
        ("actor_id", "actors", "id"),
        ("channel_id", "channels", "id"),
        ("message_id", "dispatch_messages", "dispatch_id"),
    ] {
        fk(&mut event, "events", column, target, key);
    }
    manager.create_table(event.to_owned()).await?;
    index(manager, "events", "uq_event_sequence", &["seq"], true).await?;
    index(manager, "events", "uq_event_id", &["event_id"], true).await?;
    index(
        manager,
        "events",
        "idx_event_channel_seq",
        &["channel_id", "seq"],
        false,
    )
    .await?;
    index(
        manager,
        "events",
        "idx_event_sealed_seq",
        &["sealed", "seq"],
        false,
    )
    .await?;

    let mut mentions = Table::create();
    mentions
        .table(Alias::new("mentions"))
        .col(text("message_id"))
        .col(text("recipient_id"))
        .col(json("source_kinds"))
        .primary_key(
            Index::create()
                .col(Alias::new("message_id"))
                .col(Alias::new("recipient_id")),
        );
    fk(
        &mut mentions,
        "mentions",
        "message_id",
        "dispatch_messages",
        "dispatch_id",
    );
    fk(&mut mentions, "mentions", "recipient_id", "actors", "id");
    manager.create_table(mentions.to_owned()).await?;

    let mut receipts = Table::create();
    receipts
        .table(Alias::new("read_receipts"))
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
        .check(Expr::col(Alias::new("kind")).is_in(["automatic", "explicit"]));
    fk(
        &mut receipts,
        "read_receipts",
        "message_id",
        "dispatch_messages",
        "dispatch_id",
    );
    fk(&mut receipts, "read_receipts", "actor_id", "actors", "id");
    manager.create_table(receipts.to_owned()).await?;
    index(
        manager,
        "read_receipts",
        "idx_receipt_actor_message",
        &["actor_id", "message_id"],
        false,
    )
    .await?;

    let mut coalescing = Table::create();
    coalescing
        .table(Alias::new("delivery_timeout_notices"))
        .col(text("message_id").primary_key())
        .col(text("notice_dispatch_id"));
    fk(
        &mut coalescing,
        "delivery_timeout_notices",
        "message_id",
        "dispatch_messages",
        "dispatch_id",
    );
    fk(
        &mut coalescing,
        "delivery_timeout_notices",
        "notice_dispatch_id",
        "dispatch_notices",
        "dispatch_id",
    );
    manager.create_table(coalescing.to_owned()).await?;
    index(
        manager,
        "delivery_timeout_notices",
        "uq_timeout_notice_dispatch",
        &["notice_dispatch_id"],
        true,
    )
    .await?;
    Ok(())
}
