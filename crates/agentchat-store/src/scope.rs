//! Shared recipient/channel visibility predicate for retained delivery rows.

use sea_orm::sea_query::{Alias, Condition, Expr, ExprTrait, Query, SelectStatement};
use uuid::Uuid;

/// Require current membership and a post-join Dispatch boundary for channel work.
/// Explicitly project-global Notice kinds remain eligible without a channel.
fn membership(recipient: Uuid) -> SelectStatement {
    Query::select()
        .expr(1)
        .from(Alias::new("channel_memberships"))
        .and_where(
            Expr::col((Alias::new("channel_memberships"), Alias::new("channel_id")))
                .equals((Alias::new("dispatches"), Alias::new("channel_id"))),
        )
        .and_where(
            Expr::col((Alias::new("channel_memberships"), Alias::new("actor_id")))
                .eq(recipient.to_string()),
        )
        .and_where(
            Expr::col((Alias::new("channel_memberships"), Alias::new("join_seq"))).lt(Expr::col((
                Alias::new("dispatches"),
                Alias::new("creation_seq"),
            ))),
        )
        .to_owned()
}

/// Scope a candidate that can still reach native dispatch. Unknown null-channel
/// kinds are not automatically eligible for host input.
pub(crate) fn delivery(recipient: Uuid) -> Condition {
    Condition::any()
        .add(Expr::exists(membership(recipient)))
        .add(
            Condition::all()
                .add(Expr::col((Alias::new("dispatches"), Alias::new("channel_id"))).is_null())
                .add(
                    Expr::col((Alias::new("dispatch_notices"), Alias::new("kind"))).is_in([
                        "agent.joined",
                        "user.joined",
                        "user.left",
                        "agent.left",
                        "agent.launch_retired",
                        "reservation.acquired",
                        "reservation.expired",
                        "reservation.released",
                        "project.channel_policy_changed",
                        "project.channel_policy_materialized",
                    ]),
                ),
        )
}

/// Inbox and native selection share the same forward membership boundary.
pub(crate) fn inbox_delivery(recipient: Uuid) -> Condition {
    delivery(recipient)
}
