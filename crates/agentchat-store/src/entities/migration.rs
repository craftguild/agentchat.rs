//! Immutable successful migration checksums.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "schema_migrations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i32,
    #[sea_orm(column_type = "Text")]
    pub checksum: String,
    #[sea_orm(column_type = "Text")]
    pub applied_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
