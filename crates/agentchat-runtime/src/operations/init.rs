//! Shared initialization routing and exact successful retry restoration.

use super::Operations;
use crate::{
    context::ContextRegistry,
    project::{InitActor, InitOutcome, InitRequest},
};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::operations::Target;
use serde_json::{Value, json};
use uuid::Uuid;

#[cfg(test)]
#[path = "../../tests/unit/operations_init.rs"]
mod tests;

impl Operations {
    pub(super) async fn integration_health(
        &self,
    ) -> agentchat_protocol::integration::IntegrationHealth {
        use agentchat_protocol::integration::{IntegrationHealth, LifecycleHealth};
        match &self.host_tools {
            Some(setup) => setup
                .inspect_typed()
                .await
                .unwrap_or_else(|error| IntegrationHealth {
                    state: "invalid".into(),
                    error: Some(error),
                    lifecycle: Some(LifecycleHealth::Unavailable { reason: None }),
                    tools: None,
                    reason: None,
                }),
            None => IntegrationHealth {
                state: "unavailable".into(),
                reason: Some("Host tool installation is not composed.".into()),
                lifecycle: None,
                tools: None,
                error: None,
            },
        }
    }
    pub(super) async fn integration_status(&self) -> Value {
        serde_json::to_value(self.integration_health().await)
            .expect("health DTO is JSON serializable")
    }
    pub(super) async fn install_integrations(
        &self,
        id: Uuid,
        result: &InitOutcome,
    ) -> Result<Value> {
        let mut project = json!({"project_id":result.project.identity().id,
            "scope":result.project.identity().scope,"created":result.created,
            "default_channels":result.channel_policy.ordered_defaults(),
            "primary_channel":result.channel_policy.primary()});
        if let Some(settings) = result.receipt_settings {
            project["receipt_settings"] =
                serde_json::to_value(settings).expect("typed receipt settings serialize");
        }
        let integrations = if let Some(setup) = &self.host_tools {
            setup
                .install(id, result.project.identity().id)
                .await
                .map_err(|error| {
                    // Storage/reference creation is already durable. Report that
                    // outcome even if host configuration fails; the same key may
                    // resume installation without allocating another project UUID.
                    installation_failure(error, &project)
                })?
        } else {
            json!({"state":"unavailable","reason":"Host tool installation is not composed."})
        };
        project["integrations"] = integrations;
        project["capabilities"] = agentchat_hosts::capabilities();
        Ok(project)
    }
    pub(super) async fn initialize(
        &self,
        target: Target,
        mut request: InitRequest,
    ) -> Result<InitOutcome> {
        if target.actor.is_some() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Installation does not accept operation actor selectors.",
            ));
        }
        request.receipt_settings.validate()?;
        let registry = ContextRegistry::new(self.manager.clone());
        let initialization_identity = target.initialization_identity;
        let context = if let Some(id) = target.context_id {
            Some(registry.descriptor(id).await?)
        } else {
            None
        };
        if let (Some(native), Some(context)) = (&initialization_identity, &context)
            && (native.host() != context.host
                || native.host_session_id() != context.host_session_id)
        {
            return Err(Error::new(
                ErrorCode::IdentityConflict,
                "Native initialization identity conflicts with its resolved context.",
            ));
        }
        if let Some(expected) = target.expected_project_id
            && context.as_ref().is_none_or(|c| c.project_id != expected)
        {
            return Err(Error::new(
                ErrorCode::ProjectBindingMismatch,
                "Bound initialization cannot switch its captured context namespace.",
            ));
        }
        let directory = target
            .project_path
            .or_else(|| context.as_ref().map(|c| c.working_directory.clone()))
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    "Initialization requires an explicit project path.",
                )
            })?;
        // Caller flags share the durable request type; authority fields always
        // come from this route rather than a caller-supplied actor projection.
        request.expected_project_id = context.as_ref().map(|c| c.project_id);
        request.actor = if let Some(native) = initialization_identity {
            InitActor::Conversation {
                host: native.host(),
                host_session_id: native.host_session_id().to_owned(),
            }
        } else {
            context
                .as_ref()
                .map_or(InitActor::Local, |c| InitActor::Conversation {
                    host: c.host,
                    host_session_id: c.host_session_id.clone(),
                })
        };
        if let Some(result) = self
            .manager
            .resume_initialization(&directory, request.clone())
            .await?
        {
            return Ok(result);
        }
        if let Some(context) = context {
            registry
                .resolve(context.context_id, Some(context.project_id))
                .await?;
            let existing = self.manager.discover(&directory).await?.ok_or_else(|| {
                Error::new(
                    ErrorCode::ProjectBindingMismatch,
                    "Bound initialization must target its current project.",
                )
            })?;
            if existing.identity().id != context.project_id {
                return Err(Error::new(
                    ErrorCode::ProjectBindingMismatch,
                    "Initialization path conflicts with the bound project.",
                ));
            }
        }
        self.manager.initialize(directory, request).await
    }
}

fn installation_failure(mut error: Error, initialization: &Value) -> Error {
    let cause = std::mem::take(&mut error.details);
    error.details = json!({"initialization":initialization,
        "integrations":{"state":"failed"},"cause":cause});
    if error.code == ErrorCode::UnsupportedProtocol {
        for key in ["host", "host_version", "observed_version", "missing_api"] {
            if let Some(value) = error.details["cause"].get(key).cloned() {
                error.details[key] = value;
            }
        }
    }
    error
}
