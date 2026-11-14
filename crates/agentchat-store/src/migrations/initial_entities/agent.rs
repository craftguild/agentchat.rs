// Generated once from the initial SeaORM entity model. Do not edit released history.
//! Relational agents records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "agents")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub id: String,
    #[sea_orm(column_type = "Text")]
    pub host_kind: String,
    #[sea_orm(column_type = "Text")]
    pub host_session_id: String,
    #[sea_orm(column_type = "Text")]
    pub handle: String,
    #[sea_orm(column_type = "Text")]
    pub participation: String,
    #[sea_orm(column_type = "Text")]
    pub first_join_at: String,
    pub join_seq: i64,
    pub leave_seq: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
