//! Relational reservation_patterns records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "reservation_patterns")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub reservation_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub ordinal: i32,
    #[sea_orm(column_type = "Text")]
    pub pattern: String,
    pub grammar_version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
