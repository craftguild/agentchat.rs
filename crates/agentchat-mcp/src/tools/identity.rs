//! Invitation and channel tools preserve the core's exact identity policy.

use super::*;
#[tool_router(router=identity_tools,vis="pub(crate)")]
impl Server {
    #[tool(description="Explicitly join or rejoin this verified native conversation under its live managed launch. Native routing supplies context_id; an unbound caller must supply it. Never guesses identity from cwd.",output_schema=result::schema())]
    async fn agentchat_join(&self, Parameters(p): Parameters<params::Join>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::Join { name: p.name },
            },
        )
        .await
    }
    #[tool(description="Leave this bound conversation, cancel future deliveries and release its owned reservations.",output_schema=result::schema())]
    async fn agentchat_leave(&self, Parameters(p): Parameters<params::Plain>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::Leave,
            },
        )
        .await
    }
    #[tool(description="Inspect this verified native actor's identity/participation plus project defaults, receipt settings, statistics and runtime/integration health. This read never joins, registers an actor or refreshes host evidence.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_whoami(&self, Parameters(p): Parameters<params::Plain>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::Whoami,
            },
        )
        .await
    }
    #[tool(description="List retained joined/left identities with effective online/offline state, using finite continuation cursors.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_agents_list(
        &self,
        Parameters(p): Parameters<params::Paged>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::Agents {
                    page: PageRequest {
                        limit: p.limit,
                        after: p.after,
                    },
                },
            },
        )
        .await
    }
    #[tool(description="Create a channel without changing any participant's membership.",output_schema=result::schema())]
    async fn agentchat_channel_create(
        &self,
        Parameters(p): Parameters<params::ChannelName>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::ChannelCreate { name: p.name },
            },
        )
        .await
    }
    #[tool(description="Join this participant to a non-default channel. Repeated current membership is idempotent.",output_schema=result::schema())]
    async fn agentchat_channel_join(
        &self,
        Parameters(p): Parameters<params::ChannelName>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::ChannelJoin { name: p.name },
            },
        )
        .await
    }
    #[tool(description="Leave this participant's non-default channel membership and fence future channel delivery.",output_schema=result::schema())]
    async fn agentchat_channel_leave(
        &self,
        Parameters(p): Parameters<params::ChannelName>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::ChannelLeave { name: p.name },
            },
        )
        .await
    }
    #[tool(description="List channels by derived active, archived or all state without changing membership or read state.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_channel_list(
        &self,
        Parameters(p): Parameters<params::ChannelList>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::Channels {
                    condition: p.state.into(),
                    page: PageRequest {
                        limit: p.limit,
                        after: p.after,
                    },
                },
            },
        )
        .await
    }
    #[tool(description="List currently joined members of a channel with effective presence, including disconnected participants.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_channel_members(
        &self,
        Parameters(p): Parameters<params::Members>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::ChannelMembers {
                    channel: p.name,
                    page: PageRequest {
                        limit: p.limit,
                        after: p.after,
                    },
                },
            },
        )
        .await
    }
}
