// Generated once from the initial SeaORM entity model. Do not edit released history.
//! Relational reservations records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "reservations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub id: String,
    #[sea_orm(column_type = "Text")]
    pub owner_id: String,
    pub generation: i64,
    #[sea_orm(column_type = "Text")]
    pub state: String,
    pub ttl_seconds: i32,
    #[sea_orm(column_type = "Text")]
    pub acquired_at: String,
    #[sea_orm(column_type = "Text")]
    pub expires_at: String,
    #[sea_orm(column_type = "Text")]
    pub released_at: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub reason: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
