//! One immutable Notice Dispatch identity per original Message Dispatch.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delivery_timeout_notices")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub message_id: String,
    pub notice_dispatch_id: String,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
