//! Administrative operations have no conversation routing or destructive MCP tool.
use super::Operations;
use agentchat_core::{Error, ErrorCode};
use agentchat_protocol::{
    envelope::Envelope,
    removal::{RemovalOutcome, RemovalScope},
};
use std::path::PathBuf;
use uuid::Uuid;

impl Operations {
    /// Remove the selected scope through the common application policy. Global
    /// scope ignores project paths and needs no usable working directory/context.
    /// Local scope requires an explicit discovery path; neither scope joins.
    pub async fn uninstall(
        &self,
        id: Uuid,
        scope: RemovalScope,
        path: Option<PathBuf>,
    ) -> Envelope<RemovalOutcome> {
        let environment = self
            .host_tools
            .as_ref()
            .map(|t| t.environment())
            .unwrap_or_else(agentchat_hosts::setup::Environment::capture);
        let result = match scope {
            RemovalScope::Global => {
                self.manager
                    .uninstall_global_with_environment(id, environment)
                    .await
            }
            RemovalScope::Project => match path {
                Some(path) => {
                    self.manager
                        .uninstall_project_with_environment(path, id, environment)
                        .await
                }
                None => Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "Project uninstall requires a discovery directory.",
                )),
            },
        };
        let project = result.as_ref().ok().and_then(|r| r.project_id);
        Envelope::new(id, project, result)
    }
}
