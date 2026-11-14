// Generated once from the initial SeaORM entity model. Do not edit released history.
//! Relational channel_memberships records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "channel_memberships")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub channel_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub agent_id: String,
    pub join_seq: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
