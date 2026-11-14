//! Retain legacy Claude heads whose native outcome cannot be reconstructed.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let generations = Query::select()
            .column((Alias::new("launches"), Alias::new("generation")))
            .from(Alias::new("launches"))
            .inner_join(
                Alias::new("agents"),
                Expr::col((Alias::new("agents"), Alias::new("id")))
                    .equals((Alias::new("launches"), Alias::new("agent_id"))),
            )
            .and_where(Expr::col((Alias::new("agents"), Alias::new("host_kind"))).eq("claude"))
            .to_owned();
        // Older producers conflated thrown SDK calls with definite rejection.
        // Preserve original active head/token/cursor and permit only receipt reads;
        // neither a legacy negative nor a missing file proves non-entry.
        // Terminal cancelled generations retain their historical diagnostics.
        manager
            .get_connection()
            .execute(
                &Query::update()
                    .table(Alias::new("message_streams"))
                    .value(Alias::new("awaiting_acceptance"), true)
                    .value(Alias::new("submission_pending"), false)
                    .value(
                        Alias::new("failure_code"),
                        SimpleExpr::Value(Value::String(None)),
                    )
                    .and_where(Expr::col(Alias::new("head_delivery")).is_not_null())
                    .and_where(Expr::col(Alias::new("cancelled")).eq(false))
                    .and_where(Expr::col(Alias::new("generation")).in_subquery(generations))
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
