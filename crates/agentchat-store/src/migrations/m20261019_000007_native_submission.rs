//! Persist uncertainty before native I/O; legacy Codex heads cannot imply retry.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("message_streams"))
                    .add_column(
                        ColumnDef::new(Alias::new("submission_pending"))
                            .boolean()
                            .not_null()
                            .default(false)
                            .check(
                                Condition::any()
                                    .add(Expr::col(Alias::new("submission_pending")).eq(false))
                                    .add(
                                        Condition::all()
                                            .add(
                                                Expr::col(Alias::new("head_delivery"))
                                                    .is_not_null(),
                                            )
                                            .add(
                                                Expr::col(Alias::new("awaiting_acceptance"))
                                                    .eq(false),
                                            ),
                                    ),
                            ),
                    )
                    .to_owned(),
            )
            .await?;
        let generations = Query::select()
            .column((Alias::new("launches"), Alias::new("generation")))
            .from(Alias::new("launches"))
            .inner_join(
                Alias::new("agents"),
                Expr::col((Alias::new("agents"), Alias::new("id")))
                    .equals((Alias::new("launches"), Alias::new("agent_id"))),
            )
            .and_where(Expr::col((Alias::new("agents"), Alias::new("host_kind"))).eq("codex"))
            .to_owned();
        // Older schemas do not retain whether a Codex request reached the host.
        // Preserve every original head/token/cursor conservatively, including
        // terminal generations. Claude's existing publication receipt survives.
        manager
            .get_connection()
            .execute(
                &Query::update()
                    .table(Alias::new("message_streams"))
                    .value(Alias::new("submission_pending"), true)
                    .value(Alias::new("awaiting_acceptance"), false)
                    .and_where(Expr::col(Alias::new("head_delivery")).is_not_null())
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
