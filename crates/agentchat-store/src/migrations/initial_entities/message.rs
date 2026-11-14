// Generated once from the initial SeaORM entity model. Do not edit released history.
//! Relational messages records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "messages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub id: String,
    #[sea_orm(column_type = "Text")]
    pub channel_id: String,
    #[sea_orm(column_type = "Text")]
    pub author_id: String,
    #[sea_orm(column_type = "Text")]
    pub root_message_id: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub body: String,
    #[sea_orm(column_type = "Text")]
    pub created_at: String,
    pub creation_seq: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
