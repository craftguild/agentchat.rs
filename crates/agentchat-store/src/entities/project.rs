//! Singleton project metadata; schema compatibility is checked before writes.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "project_meta")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub singleton: i32,
    #[sea_orm(column_type = "Text")]
    pub project_id: String,
    #[sea_orm(column_type = "Text")]
    pub scope: String,
    #[sea_orm(column_type = "Text")]
    pub created_at: String,
    pub schema_version: i32,
    pub format_version: i32,
    pub next_sequence: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
