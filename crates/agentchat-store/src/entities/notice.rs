//! Required Notice payload subtype, never a Message/read target.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "dispatch_notices")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub dispatch_id: String,
    pub kind: String,
    pub data: Json,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
