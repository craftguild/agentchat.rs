//! Explicit user intent, adapted to the injected shared-operation capability.
use super::{Fields, Types, input};
use agentchat_core::{Error, ErrorCode, Result, mentions::MentionWarning};
use agentchat_protocol::viewer_write::{CreatedChannel, PostedMessage, ViewerWriter};
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, Id};

impl Fields {
    fn writer(&self, allowed: bool) -> Result<&dyn ViewerWriter> {
        self.writer.as_deref().filter(|_| allowed).ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidArgument,
                "User actions require the same-origin writable viewer.",
            )
        })
    }
}
impl contract::MutationResolver<bool, Types, Error> for Fields {
    async fn create_channel<'a>(
        &'a self,
        ctx: FieldContext<'a, bool>,
        args: contract::MutationCreateChannelArgs,
    ) -> Result<CreatedChannel> {
        self.writer(*ctx.context())?
            .create_channel(input::uuid(&args.request_id)?, args.name)
            .await
    }
    async fn post_message<'a>(
        &'a self,
        ctx: FieldContext<'a, bool>,
        args: contract::MutationPostMessageArgs,
    ) -> Result<PostedMessage> {
        self.writer(*ctx.context())?
            .post(input::uuid(&args.request_id)?, args.channel, args.body)
            .await
    }
    async fn reply_message<'a>(
        &'a self,
        ctx: FieldContext<'a, bool>,
        args: contract::MutationReplyMessageArgs,
    ) -> Result<PostedMessage> {
        self.writer(*ctx.context())?
            .reply(
                input::uuid(&args.request_id)?,
                input::uuid(&args.root_id)?,
                args.body,
            )
            .await
    }
}
impl contract::CreatedChannelResolver<bool, Types, Error> for Fields {
    async fn channel_id<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a CreatedChannel,
    ) -> Result<Id> {
        Ok(Id::new(p.channel_id.to_string()))
    }
    async fn name<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a CreatedChannel,
    ) -> Result<String> {
        Ok(p.name.clone())
    }
}
impl contract::PostedMessageResolver<bool, Types, Error> for Fields {
    async fn message_id<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a PostedMessage,
    ) -> Result<Id> {
        Ok(Id::new(p.message_id.to_string()))
    }
    async fn warnings<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a PostedMessage,
    ) -> Result<Vec<MentionWarning>> {
        Ok(p.warnings.clone())
    }
}
impl contract::PostWarningResolver<bool, Types, Error> for Fields {
    async fn code<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MentionWarning,
    ) -> Result<String> {
        Ok(p.code.clone())
    }
    async fn handle<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MentionWarning,
    ) -> Result<String> {
        Ok(p.handle.clone())
    }
}
