//! Isolated evidence composition over the same native gateway as ordinary MCP.
use super::protocol::{self, NativeProfile, Result};
use agentchat_cli::claude_gateway::{Configuration, Trace};
use agentchat_runtime::project::ProjectManager;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
struct Evidence(PathBuf);
impl Trace for Evidence {
    fn record(&self, event: serde_json::Value) {
        let _ = protocol::audit(&self.0, event);
    }
}
pub(super) async fn run(root: &Path, binary: PathBuf) -> Result<()> {
    let profile: NativeProfile = protocol::read(&root.join("native-profile.json"))?;
    let manager = ProjectManager::from_environment()?;
    let owner_hint = std::env::var("CLAUDE_CODE_SESSION_ID")?.parse()?;
    let backend = Arc::new(agentchat_cli::operations(manager.clone(), binary));
    agentchat_cli::claude_gateway::serve(
        Configuration {
            directory: root.into(),
            profile,
            owner_hint,
            owner_cursor: None,
            tool_prefix: "mcp__agentchat-g0-routing__".into(),
        },
        backend,
        manager,
        Some(Arc::new(Evidence(root.into()))),
    )
    .await
}
