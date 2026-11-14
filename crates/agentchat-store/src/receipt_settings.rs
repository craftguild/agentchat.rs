//! Typed receipt configuration access shared by writers and read snapshots.

use agentchat_core::{Result, project::ProjectReceiptSettings};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, Set};

use crate::{
    entities::project_receipt_settings as settings,
    error::{database, integrity},
};

pub(crate) async fn get<C: ConnectionTrait>(db: &C) -> Result<ProjectReceiptSettings> {
    let row = settings::Entity::find_by_id(1)
        .one(db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Project receipt settings are missing."))?;
    ProjectReceiptSettings {
        auto_read: row.auto_read,
        delivery_confirmation_timeout_seconds: u32::try_from(
            row.delivery_confirmation_timeout_seconds,
        )
        .map_err(|_| integrity("Invalid retained delivery confirmation timeout."))?,
    }
    .validate()
    .map_err(|_| integrity("Invalid retained project receipt settings."))
}

pub(crate) async fn save<C: ConnectionTrait>(db: &C, value: &ProjectReceiptSettings) -> Result<()> {
    value.validate()?;
    settings::ActiveModel {
        singleton: Set(1),
        auto_read: Set(value.auto_read),
        delivery_confirmation_timeout_seconds: Set(
            value.delivery_confirmation_timeout_seconds as i32
        ),
    }
    .update(db)
    .await
    .map_err(database)?;
    Ok(())
}
