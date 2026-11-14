//! Relational events records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub ordinal: i64,
    pub seq: i64,
    #[sea_orm(column_type = "Text")]
    pub event_id: String,
    pub version: i32,
    #[sea_orm(column_type = "Text")]
    pub kind: String,
    #[sea_orm(column_type = "Text")]
    pub occurred_at: String,
    #[sea_orm(column_type = "Text")]
    pub actor_id: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub channel_id: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub message_id: Option<String>,
    pub data: Json,
    pub sealed: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
