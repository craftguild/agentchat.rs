//! Reconstruct exact message attribution using the original saved policy.
use crate::{
    Error, ErrorCode, Result,
    delivery::{MessageInput, MessageItem},
    model::{Context, Launch},
    read::IdentityLabel,
};
pub(super) fn message(
    context: &Context,
    launch: &Launch,
    item: &MessageItem,
    policy: crate::project::InputReceiptPolicy,
) -> Result<MessageInput> {
    item.dispatch
        .validate_payload(&crate::model::DispatchPayload::Message(
            item.message.clone(),
        ))?;
    if item.message.author_id != item.author.actor_id
        || item.message.channel_id != item.channel.id
        || item.dispatch.creation_seq <= launch.start_seq
        || item.delivery.dispatch_id != item.dispatch.id
        || item.delivery.recipient_id != launch.agent_id
        || item.author.actor_id == launch.agent_id
        || item.delivery.state == crate::model::DeliveryState::FailedLeft
    {
        return Err(Error::new(
            ErrorCode::IntegrityError,
            "Invalid complete Message candidate.",
        ));
    }
    let value = MessageInput {
        project_id: context.project_id,
        generation: launch.identity.generation,
        delivery_id: item.delivery.id,
        frame_revision: crate::project::NativeFrameRevision::DispatchV4,
        dispatch_id: item.dispatch.id,
        seq: item.dispatch.creation_seq,
        message_id: item.message.id,
        thread_id: item.message.thread_id(),
        author: IdentityLabel {
            actor_id: item.author.actor_id,
            kind: item.author.kind(),
            handle: item.author.handle.clone(),
        },
        channel_id: item.channel.id,
        channel: item.channel.name.clone(),
        body: item.message.body.clone(),
        addressed: item.delivery.wake_eligible,
        auto_read: policy.auto_read,
    };
    value.text()?;
    Ok(value)
}
