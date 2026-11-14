//! Shared recipient/Dispatch selection; audit records never supply native input.
use crate::entities::{delivery, dispatch, message, notice};
use agentchat_core::sequence::Sequence;
use sea_orm::sea_query::{
    Asterisk, Condition, Expr, ExprTrait, JoinType, Order, Query, SelectStatement,
};
use uuid::Uuid;

pub(crate) fn selected(recipient: Uuid) -> SelectStatement {
    filtered(
        Query::select()
            .column((delivery::Entity, Asterisk))
            .from(delivery::Entity)
            .inner_join(
                dispatch::Entity,
                Expr::col((dispatch::Entity, dispatch::Column::Id))
                    .equals((delivery::Entity, delivery::Column::DispatchId)),
            )
            .to_owned(),
        recipient,
    )
}

/// Try only a bounded sequence prefix. A miss must use the complete selector
/// with the original cursor; this page never advances a recipient's state.
pub(crate) fn native_prefix(recipient: Uuid, after: Sequence) -> SelectStatement {
    let headers = Query::select()
        .column(Asterisk)
        .from(dispatch::Entity)
        .and_where(Expr::col(dispatch::Column::CreationSeq).gt(after.get()))
        .and_where(Expr::col(dispatch::Column::Sealed).eq(true))
        .order_by(dispatch::Column::CreationSeq, Order::Asc)
        .limit(64)
        .to_owned();
    // SQLite preserves CROSS JOIN order. The derived table bounds the outer
    // sequence walk, while the recipient lookup uses the existing unique key.
    filtered(
        Query::select()
            .column((delivery::Entity, Asterisk))
            .from_subquery(headers, dispatch::Entity)
            .cross_join(delivery::Entity)
            .and_where(
                Expr::col((dispatch::Entity, dispatch::Column::Id))
                    .equals((delivery::Entity, delivery::Column::DispatchId)),
            )
            .to_owned(),
        recipient,
    )
}

fn filtered(mut query: SelectStatement, recipient: Uuid) -> SelectStatement {
    query
        .join(
            JoinType::LeftJoin,
            message::Entity,
            Expr::col((message::Entity, message::Column::DispatchId))
                .equals((dispatch::Entity, dispatch::Column::Id)),
        )
        .join(
            JoinType::LeftJoin,
            notice::Entity,
            Expr::col((notice::Entity, notice::Column::DispatchId))
                .equals((dispatch::Entity, dispatch::Column::Id)),
        )
        .and_where(
            Expr::col((delivery::Entity, delivery::Column::RecipientId)).eq(recipient.to_string()),
        )
        .and_where(
            Expr::col((delivery::Entity, delivery::Column::State)).is_in(["pending", "processing"]),
        )
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Sealed)).eq(true))
        .cond_where(
            Condition::any()
                .add(Expr::col((dispatch::Entity, dispatch::Column::Kind)).eq("notice"))
                .add(
                    Expr::col((message::Entity, message::Column::AuthorId))
                        .ne(recipient.to_string()),
                ),
        )
        .to_owned()
}
