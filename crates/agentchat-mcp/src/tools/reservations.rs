//! Exact-generation cooperative reservation tool adaptations.

use super::*;
#[tool_router(router=reservation_tools,vis="pub(crate)")]
impl Server {
    #[tool(description="Atomically acquire the complete logical pattern group or acquire nothing. Exact overlap includes nonexistent paths; reservations are cooperative.",output_schema=result::schema())]
    async fn agentchat_reserve(
        &self,
        Parameters(p): Parameters<params::Reserve>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::Reserve {
                    patterns: p.patterns,
                    ttl_seconds: p.ttl,
                    reason: p.reason,
                },
            },
        )
        .await
    }
    #[tool(description="List active cooperative groups by default; all includes retained released/expired states. Retrieval never renews a lease.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_reservations_list(
        &self,
        Parameters(p): Parameters<params::Reservations>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Query {
                query: Query::Reservations {
                    owner: p.owner,
                    all: p.all,
                    page: PageRequest {
                        limit: p.limit,
                        after: p.after,
                    },
                },
            },
        )
        .await
    }
    #[tool(description="Renew an owned active reservation using its exact acquisition generation. Expired or released groups cannot be revived.",output_schema=result::schema())]
    async fn agentchat_reservations_renew(
        &self,
        Parameters(p): Parameters<params::Lease>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::ReservationsRenew {
                    reservation_id: p.reservation_id,
                    generation: p.generation,
                },
            },
        )
        .await
    }
    #[tool(description="Release an owned group using its exact generation. Repeating the same-generation release is idempotent.",output_schema=result::schema())]
    async fn agentchat_reservations_release(
        &self,
        Parameters(p): Parameters<params::Lease>,
    ) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Mutate {
                command: Mutation::ReservationsRelease {
                    reservation_id: p.reservation_id,
                    generation: p.generation,
                },
            },
        )
        .await
    }
}
