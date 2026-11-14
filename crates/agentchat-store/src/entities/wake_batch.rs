//! Relational wake_batches records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "wake_batches")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub id: String,
    #[sea_orm(column_type = "Text")]
    pub recipient_id: String,
    #[sea_orm(column_type = "Text")]
    pub incarnation: String,
    #[sea_orm(column_type = "Text")]
    pub state: String,
    #[sea_orm(column_type = "Text")]
    pub dispatch_ref: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub turn_ref: Option<String>,
    pub attempts: i32,
    #[sea_orm(column_type = "Text")]
    pub next_eligible_at: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub failure_code: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
