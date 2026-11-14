//! Conversation payload subtype; shared Header attributes are never duplicated.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "dispatch_messages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    #[sea_orm(column_type = "Text")]
    pub dispatch_id: String,
    #[sea_orm(column_type = "Text")]
    pub author_id: String,
    #[sea_orm(column_type = "Text")]
    pub root_message_id: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub body: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
