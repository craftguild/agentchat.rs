//! Relational read_receipts records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "read_receipts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub message_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub actor_id: String,
    #[sea_orm(column_type = "Text")]
    #[sea_orm(primary_key, auto_increment = false)]
    pub kind: String,
    pub first_read_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
