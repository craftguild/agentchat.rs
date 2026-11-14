//! Effective project receipt configuration; never an inflight input snapshot.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "project_receipt_settings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub singleton: i32,
    pub auto_read: bool,
    pub delivery_confirmation_timeout_seconds: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
