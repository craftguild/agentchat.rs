//! Literal message, retrieval and independent read adaptations.

use super::*;
#[tool_router(router=message_tools,vis="pub(crate)")]
impl Server {
    #[tool(description="Post a complete immutable root message. An omitted channel resolves to the project's primary default. Explicit mentions select wake recipients; text is never evaluated as a shell command.",output_schema=result::schema())]
    async fn agentchat_post(&self, Parameters(p): Parameters<params::Post>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::Post {
                    channel: p.channel,
                    body: p.body,
                },
            },
        )
        .await
    }
    #[tool(description="Reply directly to a root in its channel; optional read_message_ids marks only bodies actually read in the same atomic operation. Replies do not implicitly mention the root author.",output_schema=result::schema())]
    async fn agentchat_reply(&self, Parameters(p): Parameters<params::Reply>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::Reply {
                    read_message_ids: p.read_message_ids.unwrap_or_default(),
                    root_message_id: p.root_message_id,
                    body: p.body,
                },
            },
        )
        .await
    }
    #[tool(description="Fetch a complete message and canonical paged recipient receipts, including typed delivery state, timestamp history and automatic/explicit read kinds. Retrieval creates no receipt.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_message(
        &self,
        Parameters(p): Parameters<params::Message>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::Message {
                    message_id: p.message_id,
                    receipts: PageRequest {
                        limit: p.limit,
                        after: p.after,
                    },
                },
            },
        )
        .await
    }
    #[tool(description="Retrieve a finite complete-body history page in creation order; optional channel/thread filters must agree.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_history(
        &self,
        Parameters(p): Parameters<params::History>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::History {
                    channel: p.channel,
                    thread: p.thread,
                    page: PageRequest {
                        limit: p.limit,
                        after: p.after,
                    },
                },
            },
        )
        .await
    }
    #[tool(description="Search literal case-sensitive message substrings without query-language or wildcard interpretation and without creating receipts.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_search(&self, Parameters(p): Parameters<params::Search>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::Search {
                    query: p.query,
                    channel: p.channel,
                    thread: p.thread,
                    page: PageRequest {
                        limit: p.limit,
                        after: p.after,
                    },
                },
            },
        )
        .await
    }
    #[tool(description="Retrieve this receiving agent's pending/processing deliveries without settlement or read. Use only when needed; native input metadata states its frozen automatic policy.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_inbox(&self, Parameters(p): Parameters<params::Paged>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::Inbox {
                    page: PageRequest {
                        limit: p.limit,
                        after: p.after,
                    },
                },
            },
        )
        .await
    }
    #[tool(description="Optionally record explicit reading of these exact complete message bodies. Preserves the explicit kind's first time independently of automatic reads; never cascades or accepts work.",output_schema=result::schema())]
    async fn agentchat_read(&self, Parameters(p): Parameters<params::Read>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::Read {
                    message_ids: p.message_ids,
                },
            },
        )
        .await
    }
    #[tool(description="Fetch a finite passive event page and durable scan cursor. This is not a permanently blocking watch and never joins, acknowledges or marks read.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_events(&self, Parameters(p): Parameters<params::Events>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::Events {
                    channel: p.channel,
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
