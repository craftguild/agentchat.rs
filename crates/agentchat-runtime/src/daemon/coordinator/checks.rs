//! A hook fence reads committed state without waiting for native-bound commands.
use super::{Answer, session};
use crate::{
    context::ContextRegistry,
    project::{Project, ProjectManager},
};
use agentchat_core::{Error, ErrorCode, Result, application::Application, model::Context};
use std::sync::Arc;
use tokio::sync::watch;
use uuid::Uuid;

pub(super) struct Checks {
    app: Arc<Application>,
    manager: ProjectManager,
    project: Project,
    registry: ContextRegistry,
    stopping: watch::Receiver<bool>,
}
impl Checks {
    pub fn new(
        app: Arc<Application>,
        manager: ProjectManager,
        project: Project,
        stopping: watch::Receiver<bool>,
    ) -> Self {
        Self {
            app,
            registry: ContextRegistry::new(manager.clone()),
            manager,
            project,
            stopping,
        }
    }
    pub async fn execute(
        &self,
        call: &agentchat_protocol::rpc::Call,
        bound: Option<Uuid>,
    ) -> Result<Answer> {
        if *self.stopping.borrow() {
            return Err(super::super::unavailable("Runtime is stopping."));
        }
        let context = context(
            &self.manager,
            &self.project,
            &self.registry,
            call.context_id(),
            bound,
        )
        .await?
        .ok_or_else(session)?;
        let current = match call {
            agentchat_protocol::rpc::Call::CheckDelivery {
                batch_id,
                token,
                delivery_ids,
                ..
            } => {
                self.app
                    .wake_is_current(&context, *batch_id, *token, delivery_ids)
                    .await?
            }
            agentchat_protocol::rpc::Call::CheckMessage {
                generation,
                delivery_id,
                token,
                ..
            } => {
                self.app
                    .message_is_current(&context, *generation, *token, *delivery_id)
                    .await?
            }
            agentchat_protocol::rpc::Call::CheckNative {
                generation,
                delivery_id,
                token,
                ..
            } => {
                self.app
                    .native_is_current(&context, *generation, *token, *delivery_id)
                    .await?
            }
            _ => {
                return Err(Error::new(
                    ErrorCode::UnsupportedProtocol,
                    "Expected a native claim check.",
                ));
            }
        };
        if *self.stopping.borrow() {
            return Err(super::super::unavailable("Runtime is stopping."));
        }
        Ok(Answer {
            result: serde_json::json!({"current":current}).into(),
            subscription: None,
            bind: false,
        })
    }
}
pub(super) async fn context(
    manager: &ProjectManager,
    project: &Project,
    registry: &ContextRegistry,
    given: Option<Uuid>,
    bound: Option<Uuid>,
) -> Result<Option<Context>> {
    manager.validate(project).await?;
    if let (Some(bound), Some(given)) = (bound, given)
        && bound != given
    {
        return Err(Error::new(
            ErrorCode::IdentityConflict,
            "A live connection cannot switch its exact context.",
        ));
    }
    if let Some(id) = given.or(bound) {
        Ok(Some(
            registry.resolve(id, Some(project.identity().id)).await?,
        ))
    } else {
        Ok(None)
    }
}
