//! Retained native launch authority; generated initial models stay frozen.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "launches")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub generation: String,
    #[sea_orm(column_type = "Text")]
    pub agent_id: String,
    #[sea_orm(column_type = "Text")]
    pub context_id: String,
    #[sea_orm(column_type = "Text")]
    pub incarnation: String,
    #[sea_orm(column_type = "Text")]
    pub owner: String,
    pub start_seq: i64,
    #[sea_orm(column_type = "Text")]
    pub started_at: String,
    #[sea_orm(column_type = "Text")]
    pub retired_at: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub retirement_reason: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
