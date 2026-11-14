//! Native per-call routing over shared application operations, without admission.
pub(crate) mod configured;
mod diagnostics;
mod endpoint;
mod maintenance;
use diagnostics::Diagnostics;
pub use diagnostics::Trace;
mod projection;
pub mod records;
mod restoration;
mod server;
use records as protocol;

use agentchat_hosts::claude::observer::NativeProfile;
use agentchat_protocol::operations::Backend;
use agentchat_runtime::project::ProjectManager;
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

/// Explicit trusted executable composition, independent from model tool inputs.
pub struct Configuration {
    /// Private physical-owner metadata directory; never a project identity.
    pub directory: PathBuf,
    /// Exact native image/version expected by the observer and callback adapter.
    pub profile: NativeProfile,
    /// Opaque physical namespace locator; never current conversation authority.
    pub owner_hint: Uuid,
    /// Durable native intent/ordering scope for installed composition. The
    /// isolated feasibility gateway may retain its independently proved ledger.
    pub owner_cursor: Option<agentchat_hosts::claude::namespace::OwnerNamespace>,
    /// Exact host-native MCP tool prefix, such as `mcp__agentchat__`.
    pub tool_prefix: String,
}
/// Serve an identity-free physical MCP transport, pinning its real native
/// ancestor before publication. Native callbacks and one-use fresh per-call
/// proof select immutable attachments; this never admits a host or joins it.
/// Parent loss/stdio EOF closes attachments and only this endpoint's publication.
pub async fn serve(
    config: Configuration,
    backend: Arc<dyn Backend>,
    manager: ProjectManager,
    trace: Option<Arc<dyn Trace>>,
) -> records::Result<()> {
    if !agentchat_hosts::claude::module::candidate_version(&config.profile.version) {
        return Err(agentchat_core::Error::new(
            agentchat_core::ErrorCode::UnsupportedHost,
            "This native gateway engine version is unsupported.",
        )
        .into());
    }
    if config.owner_hint.is_nil()
        || !config.tool_prefix.starts_with("mcp__")
        || !config.tool_prefix.ends_with("__")
        || config.tool_prefix.len() < 8
        || config.tool_prefix.len() > 128
        || !config
            .tool_prefix
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(agentchat_core::Error::new(
            agentchat_core::ErrorCode::InvalidArgument,
            "The native gateway startup locator or tool prefix is invalid.",
        )
        .into());
    }
    server::run(config, backend, manager, Diagnostics::new(trace)).await
}
