// Generated once from the initial SeaORM entity model. Do not edit released history.
//! Relational deliveries records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "deliveries")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub id: String,
    #[sea_orm(column_type = "Text")]
    pub event_id: String,
    #[sea_orm(column_type = "Text")]
    pub recipient_id: String,
    #[sea_orm(column_type = "Text")]
    pub state: String,
    pub wake_eligible: bool,
    #[sea_orm(column_type = "Text")]
    pub wake_batch_id: Option<String>,
    pub attempts: i32,
    #[sea_orm(column_type = "Text")]
    pub acknowledged_at: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
