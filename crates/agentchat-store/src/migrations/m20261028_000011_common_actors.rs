//! Preserve native UUIDs while replacing agent-only identity with typed actors.

use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let enforcing: i64 = db
            .query_one(
                &Query::select()
                    .column(Alias::new("foreign_keys"))
                    .from_function(
                        Func::cust(Alias::new("pragma_foreign_keys")),
                        Alias::new("settings"),
                    )
                    .to_owned(),
            )
            .await?
            .ok_or_else(|| DbErr::Custom("Missing migration connection settings.".into()))?
            .try_get("", "foreign_keys")?;
        if enforcing != 0 {
            return Err(DbErr::Custom(
                "Actor reconstruction requires the guarded schema connection.".into(),
            ));
        }
        // Follow SQLite's create/copy/drop/rename reconstruction order. The
        // guarded opener holds the migration lock and immediate transaction,
        // publishes the original-schema backup first, and checks every FK before
        // committing. This connection never handles application operations.
        let table = Alias::new("actor_rebuild_v11");
        let mut schema = Table::create();
        schema
            .table(table.clone())
            .col(
                ColumnDef::new(Alias::new("id"))
                    .text()
                    .not_null()
                    .primary_key(),
            )
            .col(ColumnDef::new(Alias::new("kind")).text().not_null())
            .col(ColumnDef::new(Alias::new("host_kind")).text())
            .col(ColumnDef::new(Alias::new("host_session_id")).text())
            .col(ColumnDef::new(Alias::new("handle")).text().not_null())
            .col(
                ColumnDef::new(Alias::new("participation"))
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(Alias::new("first_join_at"))
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(Alias::new("join_seq"))
                    .big_integer()
                    .not_null(),
            )
            .col(ColumnDef::new(Alias::new("leave_seq")).big_integer())
            .check(Expr::col(Alias::new("participation")).is_in(["joined", "left"]))
            .check(
                Cond::any()
                    .add(
                        Cond::all()
                            .add(Expr::col(Alias::new("kind")).eq("agent"))
                            .add(Expr::col(Alias::new("host_kind")).is_not_null())
                            .add(Expr::col(Alias::new("host_kind")).is_in(["codex", "claude"]))
                            .add(Expr::col(Alias::new("host_session_id")).is_not_null())
                            .add(
                                Func::cust(Alias::new("length"))
                                    .arg(
                                        Expr::col(Alias::new("host_session_id"))
                                            .cast_as(Alias::new("blob")),
                                    )
                                    .between(1, 1_024),
                            ),
                    )
                    .add(
                        Cond::all()
                            .add(Expr::col(Alias::new("kind")).eq("user"))
                            .add(Expr::col(Alias::new("host_kind")).is_null())
                            .add(Expr::col(Alias::new("host_session_id")).is_null())
                            .add(Expr::col(Alias::new("handle")).eq("user")),
                    ),
            );
        for column in ["join_seq", "leave_seq"] {
            schema.foreign_key(
                ForeignKey::create()
                    .from(table.clone(), Alias::new(column))
                    .to(Alias::new("events"), Alias::new("seq")),
            );
        }
        manager.create_table(schema.to_owned()).await?;
        let retained = [
            "host_kind",
            "host_session_id",
            "handle",
            "participation",
            "first_join_at",
            "join_seq",
            "leave_seq",
        ];
        db.execute(
            &Query::insert()
                .into_table(table.clone())
                .columns(["id", "kind"].into_iter().chain(retained).map(Alias::new))
                .select_from(
                    Query::select()
                        .column(Alias::new("id"))
                        .expr(Expr::val("agent"))
                        .columns(retained.map(Alias::new))
                        .from(Alias::new("agents"))
                        .to_owned(),
                )
                .map_err(|error| DbErr::Custom(error.to_string()))?
                .to_owned(),
        )
        .await?;
        manager
            .drop_table(Table::drop().table(Alias::new("agents")).to_owned())
            .await?;
        manager
            .rename_table(
                Table::rename()
                    .table(table, Alias::new("agents"))
                    .to_owned(),
            )
            .await?;
        // A second, ordinary rename propagates all retained child FKs to the
        // final actor table. No child row or historical ownership UUID changes.
        manager
            .rename_table(
                Table::rename()
                    .table(Alias::new("agents"), Alias::new("actors"))
                    .to_owned(),
            )
            .await?;
        for (name, columns, kind) in [
            (
                "uq_actor_conversation",
                vec!["host_kind", "host_session_id"],
                "agent",
            ),
            ("uq_actor_agent_handle", vec!["handle"], "agent"),
            ("uq_actor_project_user", vec!["kind"], "user"),
        ] {
            let mut index = Index::create();
            index
                .name(name)
                .table(Alias::new("actors"))
                .unique()
                .and_where(Expr::col(Alias::new("kind")).eq(kind));
            for column in columns {
                index.col(Alias::new(column));
            }
            manager.create_index(index.to_owned()).await?;
        }
        for table in ["channel_memberships", "read_receipts"] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(table))
                        .rename_column(Alias::new("agent_id"), Alias::new("actor_id"))
                        .to_owned(),
                )
                .await?;
        }
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
