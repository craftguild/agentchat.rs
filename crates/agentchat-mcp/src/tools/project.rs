//! Shared project initialization and diagnostic tool adaptations.

use super::*;
#[tool_router(router=project_tools,vis="pub(crate)")]
impl Server {
    #[tool(description="Install project storage and host integrations through the public agentchat install workflow. Optional default_channels replaces the ordered mandatory project defaults; omission preserves an existing policy and initializes a new or legacy-unset project to dev. Optional auto_read and delivery_confirmation_timeout_seconds update project receipt settings; null/omission preserves existing values, including false. New-project defaults are auto_read true and processing timeout 3600 seconds. Unsupported old formats require explicit new_project. Reports effective settings and separate installation/capability status. Requires project_path before binding.",output_schema=result::schema())]
    async fn agentchat_init(&self, Parameters(p): Parameters<params::Init>) -> CallToolResult {
        self.call(
            p.target(),
            p.request_id,
            Operation::Initialize {
                scope: p.scope.map(Into::into),
                default_channels: p.default_channels,
                auto_read: p.auto_read,
                delivery_confirmation_timeout_seconds: p.delivery_confirmation_timeout_seconds,
                relocate: p.relocate,
                new_project: p.new_project,
            },
        )
        .await
    }
    #[tool(description="Check integrity, schema, SQLite, socket and host capabilities without repair or trust approval.",output_schema=result::schema(),annotations(read_only_hint=true))]
    async fn agentchat_doctor(&self, Parameters(p): Parameters<params::Plain>) -> CallToolResult {
        let mut result = self.call(p.target(), p.request_id, Operation::Doctor).await;
        if let Some(timings) = self.timings.snapshot() {
            // Transport-local observations belong in protocol metadata. Keep
            // the shared CLI/MCP domain envelope exactly the same.
            result
                .meta
                .get_or_insert_with(Default::default)
                .0
                .insert("agentchat.rs/native_call_timings".into(), timings);
        }
        result
    }
}
