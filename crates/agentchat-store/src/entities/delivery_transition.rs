//! Append-only initial and actual delivery state changes.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delivery_state_transitions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub delivery_id: String,
    pub ordinal: i64,
    pub from_state: Option<String>,
    pub to_state: String,
    pub changed_at: String,
    pub deadline_at: Option<String>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
