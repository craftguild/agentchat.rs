//! Relational business schema, constraints and immediate FK integrity.

use sea_orm::sea_query::{Alias, Expr, ExprTrait, ForeignKey, Func, TableCreateStatement};

use crate::migrations::v1_tables::table;
use sea_orm_migration::{DbErr, SchemaManager};

fn foreign(table: &mut TableCreateStatement, from: &str, column: &str, to: &str, target: &str) {
    table.foreign_key(
        ForeignKey::create()
            .from(Alias::new(from), Alias::new(column))
            .to(Alias::new(to), Alias::new(target)),
    );
}

fn states(table: &mut TableCreateStatement, column: &str, values: &[&str]) {
    table.check(Expr::col(Alias::new(column)).is_in(values.iter().copied()));
}

fn bytes(column: &str) -> Expr {
    Func::cust(Alias::new("length"))
        .arg(Expr::col(Alias::new(column)).cast_as(Alias::new("blob")))
        .into()
}

pub(crate) async fn create(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let mut agents = table("agents")?;
    states(&mut agents, "host_kind", &["codex", "claude"]);
    states(&mut agents, "participation", &["joined", "left"]);
    foreign(&mut agents, "agents", "join_seq", "events", "seq");
    foreign(&mut agents, "agents", "leave_seq", "events", "seq");
    manager.create_table(agents).await?;

    let mut sessions = table("sessions")?;
    states(
        &mut sessions,
        "state",
        &[
            "disconnected",
            "idle",
            "active",
            "approval_blocked",
            "paused",
        ],
    );
    foreign(&mut sessions, "sessions", "agent_id", "agents", "id");
    manager.create_table(sessions).await?;

    let mut channels = table("channels")?;
    foreign(&mut channels, "channels", "creator_id", "agents", "id");
    foreign(&mut channels, "channels", "creation_seq", "events", "seq");
    manager.create_table(channels).await?;

    let mut members = table("channel_memberships")?;
    foreign(
        &mut members,
        "channel_memberships",
        "channel_id",
        "channels",
        "id",
    );
    foreign(
        &mut members,
        "channel_memberships",
        "agent_id",
        "agents",
        "id",
    );
    foreign(
        &mut members,
        "channel_memberships",
        "join_seq",
        "events",
        "seq",
    );
    manager.create_table(members).await?;

    let mut messages = table("messages")?;
    foreign(&mut messages, "messages", "channel_id", "channels", "id");
    foreign(&mut messages, "messages", "author_id", "agents", "id");
    foreign(&mut messages, "messages", "creation_seq", "events", "seq");
    messages.index(
        sea_orm::sea_query::Index::create()
            .unique()
            .name("uq_message_channel_id")
            .col(Alias::new("channel_id"))
            .col(Alias::new("id")),
    );
    messages.foreign_key(
        ForeignKey::create()
            .from_tbl(Alias::new("messages"))
            .from_col(Alias::new("channel_id"))
            .from_col(Alias::new("root_message_id"))
            .to_tbl(Alias::new("messages"))
            .to_col(Alias::new("channel_id"))
            .to_col(Alias::new("id")),
    );
    messages.check(bytes("body").between(1, 65_536));
    messages.check(
        Expr::expr(Func::cust(Alias::new("trim")).arg(Expr::col(Alias::new("body")))).ne(""),
    );
    manager.create_table(messages).await?;

    let mut mentions = table("mentions")?;
    foreign(&mut mentions, "mentions", "message_id", "messages", "id");
    foreign(&mut mentions, "mentions", "recipient_id", "agents", "id");
    manager.create_table(mentions).await?;

    let mut receipts = table("read_receipts")?;
    foreign(
        &mut receipts,
        "read_receipts",
        "message_id",
        "messages",
        "id",
    );
    foreign(&mut receipts, "read_receipts", "agent_id", "agents", "id");
    manager.create_table(receipts).await?;

    let mut events = table("events")?;
    foreign(&mut events, "events", "actor_id", "agents", "id");
    foreign(&mut events, "events", "channel_id", "channels", "id");
    foreign(&mut events, "events", "message_id", "messages", "id");
    events.check(Expr::col(Alias::new("seq")).gt(0));
    events.check(Expr::col(Alias::new("version")).gt(0));
    manager.create_table(events).await?;

    let mut deliveries = table("deliveries")?;
    states(
        &mut deliveries,
        "state",
        &["pending", "inflight", "acknowledged", "cancelled"],
    );
    foreign(
        &mut deliveries,
        "deliveries",
        "event_id",
        "events",
        "event_id",
    );
    foreign(
        &mut deliveries,
        "deliveries",
        "recipient_id",
        "agents",
        "id",
    );
    foreign(
        &mut deliveries,
        "deliveries",
        "wake_batch_id",
        "wake_batches",
        "id",
    );
    deliveries.check(Expr::col(Alias::new("attempts")).gte(0));
    manager.create_table(deliveries).await?;

    let mut batches = table("wake_batches")?;
    states(
        &mut batches,
        "state",
        &[
            "pending",
            "inflight",
            "stalled",
            "acknowledged",
            "cancelled",
        ],
    );
    foreign(&mut batches, "wake_batches", "recipient_id", "agents", "id");
    foreign(
        &mut batches,
        "wake_batches",
        "incarnation",
        "sessions",
        "incarnation",
    );
    batches.check(Expr::col(Alias::new("attempts")).between(0, 3));
    manager.create_table(batches).await?;

    let mut reservations = table("reservations")?;
    states(
        &mut reservations,
        "state",
        &["active", "released", "expired"],
    );
    foreign(
        &mut reservations,
        "reservations",
        "owner_id",
        "agents",
        "id",
    );
    reservations.check(Expr::col(Alias::new("generation")).gt(0));
    reservations.check(Expr::col(Alias::new("ttl_seconds")).between(30, 3_600));
    reservations.check(
        Expr::col(Alias::new("reason"))
            .is_null()
            .or(bytes("reason").lte(1_024)),
    );
    manager.create_table(reservations).await?;

    let mut patterns = table("reservation_patterns")?;
    foreign(
        &mut patterns,
        "reservation_patterns",
        "reservation_id",
        "reservations",
        "id",
    );
    patterns.check(Expr::col(Alias::new("ordinal")).between(0, 31));
    patterns.check(Expr::col(Alias::new("grammar_version")).eq(1));
    patterns.check(bytes("pattern").between(1, 1_024));
    manager.create_table(patterns).await?;
    manager.create_table(table("idempotency")?).await?;
    Ok(())
}
