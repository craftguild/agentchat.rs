//! Generated traits adapt typed data without dynamic read DTO reconstruction.

mod actors;
mod agents;
mod changes;
mod input;
mod message_record;
mod messages;
mod mutation;
mod objects;
mod query;
mod receipts;
mod subscription;

use crate::{projection, render::Renderer};
use agentchat_core::{
    model::Channel,
    read::{
        ActorView, AgentView, ChannelPage, ChannelView, Checkpoint, IdentityLabel, MessageDetail,
        MessageView, Page, ReceiptDelivery, ReceiptRead, RecipientReceipt,
    },
};
use agentchat_protocol::viewer::ViewerReader;
use std::sync::Arc;

pub(crate) struct Types;
impl agentchat_viewer_contract::SchemaTypes for Types {
    type CreatedChannel = agentchat_protocol::viewer_write::CreatedChannel;
    type PostedMessage = agentchat_protocol::viewer_write::PostedMessage;
    type PostWarning = agentchat_core::mentions::MentionWarning;
    type Checkpoint = Checkpoint;
    type Project = Checkpoint;
    type Channel = Channel;
    type ChannelView = ChannelView;
    type ChannelPage = ChannelPage;
    type Author = IdentityLabel;
    type Agent = AgentView;
    type AgentPage = Page<AgentView>;
    type Actor = ActorView;
    type ActorPage = Page<ActorView>;
    type MessageRecord = MessageView;
    type MessageDetail = MessageDetail;
    type ReceiptPage = Page<RecipientReceipt>;
    type RecipientReceipt = RecipientReceipt;
    type DeliveryReceipt = ReceiptDelivery;
    type ReceiptRead = ReceiptRead;
    type Message = Arc<projection::Message>;
    type MessageWindow = Arc<projection::Window>;
    type PinnedRoot = Arc<projection::Pinned>;
    type Change = Arc<projection::Change>;
    type ChangeBatch = Arc<projection::Changes>;
}

#[derive(Clone)]
pub(crate) struct Fields {
    pub reader: Arc<dyn ViewerReader>,
    pub writer: Option<Arc<dyn agentchat_protocol::viewer_write::ViewerWriter>>,
    pub renderer: Renderer,
}
