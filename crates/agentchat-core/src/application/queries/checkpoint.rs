//! Restore-aware logical snapshot boundaries, validated inside one read snapshot.

use super::*;
use crate::read::Checkpoint;

pub(super) async fn capture(db: &mut dyn Snapshot, project: Uuid) -> Result<Checkpoint> {
    let head = db.head();
    at(db, project, head).await
}

pub(super) async fn at(db: &mut dyn Snapshot, project: Uuid, seq: Cursor) -> Result<Checkpoint> {
    let event_id = db.event_identity(seq).await?;
    if seq > db.head() || (seq != Cursor::START && event_id.is_none()) {
        return Err(invalidated());
    }
    Ok(Checkpoint {
        project_id: project,
        seq,
        event_id,
    })
}

pub(super) async fn validate(
    db: &mut dyn Snapshot,
    project: Uuid,
    value: Checkpoint,
) -> Result<Checkpoint> {
    if value.project_id != project {
        let mut error = Error::new(
            ErrorCode::ProjectBindingMismatch,
            "History cursor belongs to another project.",
        );
        error.details =
            serde_json::json!({"reason":"history_cursor_invalidated","reset_required":true});
        return Err(error);
    }
    let actual = at(db, project, value.seq).await?;
    if actual != value {
        return Err(invalidated());
    }
    Ok(value)
}

pub(super) async fn snapshot(
    db: &mut dyn Snapshot,
    project: Uuid,
    supplied: Option<Checkpoint>,
) -> Result<Checkpoint> {
    match supplied {
        Some(value) => validate(db, project, value).await,
        None => capture(db, project).await,
    }
}

pub(super) fn invalidated() -> Error {
    let mut error = Error::new(
        ErrorCode::InvalidArgument,
        "Retained history no longer matches the cursor; reset the snapshot.",
    );
    error.details =
        serde_json::json!({"reason":"history_cursor_invalidated","reset_required":true});
    error
}
