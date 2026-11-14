//! Human intent orchestrates existing shared actions; viewing never calls these.

use super::RuntimeViewer;
use agentchat_core::{Error, ErrorCode, Result, application::Mutation, read::requests};
use agentchat_protocol::{
    rpc::{ActorReference, Call, success},
    viewer_write::{CreatedChannel, PostedMessage, ViewerWriter},
};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

impl RuntimeViewer {
    async fn user_mutation(&self, id: Uuid, command: Mutation) -> Result<Value> {
        // connect revalidates the captured namespace on every step. No browser
        // actor, directory or native context can retarget this capability.
        success(
            self.connect()
                .await?
                .call(
                    id,
                    Call::Mutate {
                        request_id: id,
                        actor: ActorReference::User {},
                        command,
                    },
                )
                .await?,
        )
    }
    async fn join_user(&self) -> Result<()> {
        self.user_mutation(Uuid::new_v4(), Mutation::Join { name: None })
            .await?;
        Ok(())
    }
    async fn join_channel(&self, name: String) -> Result<()> {
        self.user_mutation(Uuid::new_v4(), Mutation::ChannelJoin { name })
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ViewerWriter for RuntimeViewer {
    async fn create_channel(&self, request_id: Uuid, name: String) -> Result<CreatedChannel> {
        let command = Mutation::ChannelCreate { name }.normalize()?;
        self.join_user().await?;
        let created: CreatedChannel = decode(self.user_mutation(request_id, command).await?)?;
        self.join_channel(created.name.clone()).await?;
        Ok(created)
    }
    async fn post(&self, request_id: Uuid, channel: String, body: String) -> Result<PostedMessage> {
        let command = Mutation::Post {
            channel: Some(channel.clone()),
            body,
        }
        .normalize()?;
        self.join_user().await?;
        self.join_channel(channel).await?;
        decode(self.user_mutation(request_id, command).await?)
    }
    async fn reply(&self, request_id: Uuid, root: Uuid, body: String) -> Result<PostedMessage> {
        let command = Mutation::Reply {
            read_message_ids: Vec::new(),
            root_message_id: root,
            body,
        }
        .normalize()?;
        let detail = self
            .read(requests::Message {
                message_id: root,
                receipts: Default::default(),
            })
            .await?;
        if detail.message.message.root_message_id.is_some() {
            return Err(Error::new(
                ErrorCode::InvalidThread,
                "Reply to a thread root.",
            ));
        }
        self.join_user().await?;
        self.join_channel(detail.message.channel).await?;
        decode(self.user_mutation(request_id, command).await?)
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| {
        Error::new(
            ErrorCode::IntegrityError,
            "Shared user action returned an invalid result.",
        )
    })
}
