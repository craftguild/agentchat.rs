//! Common relational actor identity; native metadata is absent for anonymous users.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "actors")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub id: String,
    #[sea_orm(column_type = "Text")]
    pub kind: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub host_kind: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub host_session_id: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub handle: String,
    #[sea_orm(column_type = "Text")]
    pub participation: String,
    #[sea_orm(column_type = "Text")]
    pub first_join_at: String,
    pub join_seq: i64,
    pub leave_seq: Option<i64>,
    pub timeout_notice_after_seq: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
