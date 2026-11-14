//! Ordered project default-channel records; all access goes through SeaORM.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "project_default_channels")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub position: i32,
    #[sea_orm(column_type = "Text", unique)]
    pub channel_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
