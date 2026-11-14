// Generated once from the initial SeaORM entity model. Do not edit released history.
//! Relational idempotency records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "idempotency")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub request_id: String,
    #[sea_orm(column_type = "Text")]
    pub actor_key: String,
    #[sea_orm(column_type = "Text")]
    pub operation: String,
    #[sea_orm(column_type = "Text")]
    pub argument_hash: String,
    pub result: Json,
    #[sea_orm(column_type = "Text")]
    pub committed_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
