//! Relational sessions records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub incarnation: String,
    #[sea_orm(column_type = "Text")]
    pub agent_id: String,
    #[sea_orm(column_type = "Text")]
    pub context_id: String,
    #[sea_orm(column_type = "Text")]
    pub state: String,
    #[sea_orm(column_type = "Text")]
    pub last_heartbeat: String,
    #[sea_orm(column_type = "Text")]
    pub active_turn_id: Option<String>,
    pub wake_paused: bool,
    pub is_current: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
