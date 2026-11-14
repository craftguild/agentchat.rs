//! Input parsing only; shared application operations retain all retrieval policy.

use agentchat_core::{
    Error, ErrorCode, Result,
    read::{Checkpoint, WindowRequest},
    sequence::Cursor,
};
use agentchat_viewer_contract::CheckpointInput;
use chigiri::{Id, Input};
use uuid::Uuid;

pub(super) fn optional<T>(input: Input<T>) -> Option<T> {
    match input {
        Input::Value(value) => Some(value),
        Input::Undefined | Input::Null => None,
    }
}
pub(super) fn uuid(id: &Id) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|_| Error::new(ErrorCode::InvalidArgument, "Invalid UUID."))
}
pub(super) fn checkpoint(input: &CheckpointInput) -> Result<Checkpoint> {
    Ok(Checkpoint {
        project_id: uuid(&input.project_id)?,
        seq: Cursor::parse(&input.seq)?,
        event_id: match &input.event_id {
            Input::Value(id) => Some(uuid(id)?),
            _ => None,
        },
    })
}
pub(super) fn snapshot(input: Input<CheckpointInput>) -> Result<Option<Checkpoint>> {
    optional(input).as_ref().map(checkpoint).transpose()
}
pub(super) fn limit(input: Input<i32>) -> Result<Option<usize>> {
    optional(input)
        .map(|n| {
            usize::try_from(n)
                .map_err(|_| Error::new(ErrorCode::InvalidArgument, "Invalid page limit."))
        })
        .transpose()
}
pub(super) fn cursor(input: Input<String>) -> Result<Option<Cursor>> {
    optional(input).as_deref().map(Cursor::parse).transpose()
}
pub(super) fn window(
    before: Input<String>,
    head: Input<CheckpointInput>,
    count: Input<i32>,
) -> Result<WindowRequest> {
    Ok(WindowRequest {
        before: cursor(before)?,
        snapshot: snapshot(head)?,
        limit: limit(count)?,
    })
}
