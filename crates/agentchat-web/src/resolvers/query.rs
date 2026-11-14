//! Every GraphQL query calls the injected typed shared reader.

use super::{Fields, Types, input};
use crate::projection::{Changes, Pinned, Window};
use agentchat_core::{
    Result,
    read::{
        ActorView, AgentView, ChangeRequest, ChannelListCondition, ChannelPage, Checkpoint,
        MessageDetail, Page, PageRequest, requests,
    },
    sequence::Cursor,
};
use agentchat_viewer_contract as contract;
use chigiri::FieldContext;
use std::sync::Arc;

impl contract::QueryResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn agents<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: contract::QueryAgentsArgs,
    ) -> Result<Page<AgentView>> {
        let page = PageRequest {
            after: input::cursor(args.after)?.unwrap_or(Cursor::START),
            limit: input::limit(args.limit)?,
        };
        self.reader.agents(page).await
    }
    async fn channel_members<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: contract::QueryChannelMembersArgs,
    ) -> Result<Page<ActorView>> {
        self.reader
            .channel_members(requests::ChannelMembers {
                channel: args.channel,
                page: PageRequest {
                    after: input::cursor(args.after)?.unwrap_or(Cursor::START),
                    limit: input::limit(args.limit)?,
                },
            })
            .await
    }
    async fn message<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: contract::QueryMessageArgs,
    ) -> Result<MessageDetail> {
        self.reader
            .message(requests::Message {
                message_id: input::uuid(&args.message_id)?,
                receipts: PageRequest {
                    after: input::cursor(args.after)?.unwrap_or(Cursor::START),
                    limit: input::limit(args.limit)?,
                },
            })
            .await
    }
    async fn project<'a>(&'a self, _: FieldContext<'a, bool>) -> Result<Checkpoint> {
        self.reader.head().await
    }
    async fn channels<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: contract::QueryChannelsArgs,
    ) -> Result<ChannelPage> {
        let condition = match args.state.unwrap_or(contract::ChannelState::Active) {
            contract::ChannelState::Active => ChannelListCondition::Active,
            contract::ChannelState::Archived => ChannelListCondition::Archived,
            contract::ChannelState::All => ChannelListCondition::All,
        };
        self.reader
            .channels(
                condition,
                PageRequest {
                    after: input::cursor(args.after)?.unwrap_or(Cursor::START),
                    limit: input::limit(args.limit)?,
                },
            )
            .await
    }
    async fn roots<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: contract::QueryRootsArgs,
    ) -> Result<Arc<Window>> {
        let page = self
            .reader
            .roots(requests::RootWindow {
                channel: args.channel,
                window: input::window(args.before, args.snapshot, args.limit)?,
            })
            .await?;
        Window::prepare(page, &self.renderer).await
    }
    async fn pinned_root<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: contract::QueryPinnedRootArgs,
    ) -> Result<Arc<Pinned>> {
        let page = self
            .reader
            .pinned(requests::PinnedRoot {
                channel: args.channel,
                root_message_id: input::uuid(&args.root_id)?,
                snapshot: input::snapshot(args.snapshot)?,
            })
            .await?;
        Ok(Pinned::prepare(page, &self.renderer).await)
    }
    async fn replies<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: contract::QueryRepliesArgs,
    ) -> Result<Arc<Window>> {
        let page = self
            .reader
            .replies(requests::ReplyWindow {
                channel: args.channel,
                root_message_id: input::uuid(&args.root_id)?,
                window: input::window(args.before, args.snapshot, args.limit)?,
            })
            .await?;
        Window::prepare(page, &self.renderer).await
    }
    async fn changes<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        args: contract::QueryChangesArgs,
    ) -> Result<Arc<Changes>> {
        let page = self
            .reader
            .changes(ChangeRequest {
                after: input::checkpoint(&args.after)?,
                through: input::snapshot(args.through)?,
                limit: input::limit(args.limit)?,
            })
            .await?;
        let (page, _) = Changes::prepare(page, &self.renderer).await?;
        Ok(page)
    }
}
