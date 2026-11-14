//! Frozen v5 row shape for exact-schema backups before the v6 column exists.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "message_streams")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub generation: String,
    pub accepted_seq: i64,
    pub head_delivery: Option<String>,
    pub head_seq: Option<i64>,
    pub incarnation: Option<String>,
    pub token: Option<String>,
    pub attempts: Option<i32>,
    pub retry_at: Option<String>,
    pub failure_code: Option<String>,
    pub cancelled: bool,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
