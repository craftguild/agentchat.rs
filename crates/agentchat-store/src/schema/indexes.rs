//! Explicit compound/partial indexes enforce cross-row uniqueness under races.

use sea_orm::sea_query::{Alias, ConditionalStatement, Expr, ExprTrait, Index};

use sea_orm_migration::{DbErr, SchemaManager};

pub(crate) async fn create(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for (name, table, columns, unique) in [
        (
            "uq_agent_conversation",
            "agents",
            vec!["host_kind", "host_session_id"],
            true,
        ),
        ("uq_agent_handle", "agents", vec!["handle"], true),
        ("uq_channel_name", "channels", vec!["name"], true),
        ("uq_channel_seq", "channels", vec!["creation_seq"], true),
        ("uq_message_seq", "messages", vec!["creation_seq"], true),
        (
            "idx_message_channel",
            "messages",
            vec!["channel_id", "creation_seq"],
            false,
        ),
        (
            "idx_message_root",
            "messages",
            vec!["root_message_id", "creation_seq"],
            false,
        ),
        ("uq_event_id", "events", vec!["event_id"], true),
        (
            "idx_event_channel",
            "events",
            vec!["channel_id", "seq"],
            false,
        ),
        (
            "uq_delivery_recipient",
            "deliveries",
            vec!["event_id", "recipient_id"],
            true,
        ),
        (
            "idx_delivery_inbox",
            "deliveries",
            vec!["recipient_id", "state", "event_id"],
            false,
        ),
        (
            "idx_reservation_expiry",
            "reservations",
            vec!["state", "expires_at"],
            false,
        ),
        (
            "idx_reservation_owner",
            "reservations",
            vec!["owner_id"],
            false,
        ),
    ] {
        let mut index = Index::create();
        index.name(name).table(Alias::new(table));
        for column in columns {
            index.col(Alias::new(column));
        }
        if unique {
            index.unique();
        }
        manager.create_index(index).await?;
    }
    let mut session = Index::create();
    session
        .name("uq_session_current")
        .table(Alias::new("sessions"))
        .col(Alias::new("agent_id"))
        .unique()
        .and_where(Expr::col(Alias::new("is_current")).eq(true));
    manager.create_index(session).await?;
    let mut batch = Index::create();
    batch
        .name("uq_batch_outstanding")
        .table(Alias::new("wake_batches"))
        .col(Alias::new("recipient_id"))
        .col(Alias::new("incarnation"))
        .unique()
        .and_where(Expr::col(Alias::new("state")).is_in(["pending", "inflight", "stalled"]));
    manager.create_index(batch).await?;
    Ok(())
}
