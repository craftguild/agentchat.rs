//! Serialized connection binding, stable retry keys and post-commit restoration.

use super::{Server, result};
use agentchat_core::{Error, ErrorCode, Result, application::Mutation};
use agentchat_protocol::{
    envelope::{Envelope, Outcome},
    operations::{Attachment, Binding, Operation, Target},
};
use rmcp::model::CallToolResult;
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

#[derive(Default)]
pub(super) struct State {
    binding: Option<Binding>,
    attachment: Option<Box<dyn Attachment>>,
    pending: Option<Pending>,
}
struct Pending {
    context: Uuid,
    id: Uuid,
    command: Mutation,
}
impl Server {
    pub(super) async fn has_binding(&self) -> bool {
        self.state.lock().await.binding.is_some()
    }
    pub(super) async fn check_bound_attachment(&self) -> Result<bool> {
        let state = self.state.try_lock().map_err(|_| busy())?;
        if self.is_retired() {
            return Err(retired());
        }
        if state.binding.is_none() {
            return Ok(false);
        }
        let attachment = state.attachment.as_ref().ok_or_else(retired)?;
        attachment.check().await?;
        if self.is_retired() {
            return Err(retired());
        }
        Ok(true)
    }
    pub(super) async fn refresh_binding(&self) -> Result<bool> {
        let state = self.state.try_lock().map_err(|_| busy())?;
        if self.is_retired() {
            return Err(retired());
        }
        let Some(attachment) = &state.attachment else {
            // A control hint must remain inert before explicit invitation or
            // verified restoration. It cannot discover/attach a project.
            return Ok(false);
        };
        tokio::select! {
            biased;
            _ = self.retirement() => Err(retired()),
            result = attachment.refresh() => {
                result?;
                if self.is_retired() { Err(retired()) } else { Ok(true) }
            }
        }
    }
    pub(super) async fn restore_binding(&self, target: Target) -> Result<()> {
        if self.is_retired() {
            return Err(retired());
        }
        let context = target.context_id.ok_or_else(retired)?;
        if self
            .required_context
            .is_some_and(|required| required != context)
        {
            return Err(identity());
        }
        let mut state = self.state.try_lock().map_err(|_| busy())?;
        if state.binding.is_some() || state.pending.is_some() {
            return Err(identity());
        }
        let expected_project = target.expected_project_id;
        let attachment = self.backend.attach(target).await?;
        if self.is_retired() {
            return Err(retired());
        }
        if attachment.binding().context_id != context
            || expected_project.is_some_and(|project| project != attachment.binding().project_id)
        {
            return Err(identity());
        }
        state.binding = Some(attachment.binding().clone());
        state.attachment = Some(attachment);
        Ok(())
    }
    pub(super) fn invalid(&self, id: Option<Uuid>, error: Error) -> CallToolResult {
        result::wrap(Envelope::new(
            id.unwrap_or_else(Uuid::new_v4),
            None,
            Err(error),
        ))
    }
    pub(super) async fn call(
        &self,
        mut target: Target,
        requested: Option<Uuid>,
        mut operation: Operation,
    ) -> CallToolResult {
        let _operation = match self.operation.try_lock() {
            Ok(guard) => guard,
            Err(_) => return self.invalid(requested, busy()),
        };
        let mut state = match self.state.try_lock() {
            Ok(s) => s,
            Err(_) => return self.invalid(requested, busy()),
        };
        if self.is_retired() {
            return self.invalid(requested, retired());
        }
        if self.bootstrap
            && matches!(
                operation,
                Operation::Mutate {
                    command: Mutation::Join { .. }
                }
            )
        {
            let command = match self.initialization_identity.as_ref().map(|id| id.host()) {
                Some(agentchat_core::model::HostKind::Claude) => "agentchat launch claude",
                _ => "agentchat launch codex",
            };
            let mut error = Error::new(
                ErrorCode::SessionRequired,
                format!("Restart this conversation with {command} before joining AgentChat."),
            );
            error.details =
                serde_json::json!({"reason":"managed_launch_required","required_command":command});
            return self.invalid(requested, error);
        }
        if self.bootstrap && target.context_id.is_some() {
            // A native bootstrap proves only its root lifecycle. It cannot
            // borrow an existing context from arguments, history or another root.
            return self.invalid(requested, identity());
        }
        if self.bootstrap && matches!(operation, Operation::Initialize { .. }) {
            // Preserve the exact native actor across initial creation and a
            // later real context. Never retry that conversation's init as Local.
            target.initialization_identity = self.initialization_identity.clone();
        }
        if self
            .required_context
            .is_some_and(|required| target.context_id.is_some_and(|given| given != required))
        {
            return self.invalid(requested, identity());
        }
        if let Some(context) = self.required_context {
            // Native verification selected this immutable descriptor. Supplying
            // it identifies an unjoined reader; ordinary validation still
            // requires an explicit project path and an explicit join invitation.
            target.context_id = Some(context);
        }
        let joining = matches!(
            operation,
            Operation::Mutate {
                command: Mutation::Join { .. }
            }
        );
        let original_pending = state.pending.is_some();
        let id = requested
            .or_else(|| {
                if joining {
                    state.pending.as_ref().map(|p| p.id)
                } else {
                    None
                }
            })
            .unwrap_or_else(Uuid::new_v4);
        let project = state.binding.as_ref().map(|b| b.project_id);
        if joining && let Operation::Mutate { command } = &mut operation {
            match command.clone().normalize() {
                Ok(normalized) => *command = normalized,
                Err(error) => return result::wrap(Envelope::new(id, project, Err(error))),
            }
        }
        let mut fail = validate(&state, &mut target, &operation, id);
        if target
            .project_path
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            fail = Err(Error::new(
                ErrorCode::InvalidArgument,
                "Project path must not be empty.",
            ));
        }
        if let Err(error) = fail {
            return result::wrap(Envelope::new(id, project, Err(error)));
        }
        if joining
            && state.binding.is_none()
            && state.pending.is_none()
            && let Operation::Mutate { command } = &operation
        {
            // Install intent before awaiting commit. Cancellation cannot make
            // another conversation inherit this same live MCP connection.
            state.pending = Some(Pending {
                context: target.context_id.expect("validated join context"),
                id,
                command: command.clone(),
            });
        }
        let retryable_operation = matches!(
            operation,
            Operation::Query { .. } | Operation::Mutate { .. }
        );
        // Native rejoin can synchronously publish a fresh lifecycle. Its cleanup
        // must acquire state while this operation awaits the backend. Keep call
        // serialization separately; never hold the binding mutex across it.
        drop(state);
        let mut envelope = self
            .backend
            .invoke(id, target.clone(), operation.clone())
            .await;
        for _ in 0..2 {
            if self.is_retired() {
                break;
            }
            let retry = matches!(&envelope.outcome,Outcome::Failure{error} if retryable_operation &&
                (error.code==ErrorCode::IoError||error.retryable&&matches!(error.code,ErrorCode::Busy|ErrorCode::RuntimeUnavailable)));
            if !retry {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            if self.is_retired() {
                break;
            }
            envelope = self
                .backend
                .invoke(id, target.clone(), operation.clone())
                .await;
        }
        let mut state = self.state.lock().await;
        if joining && !self.is_retired() {
            if let Outcome::Success { result } = &envelope.outcome {
                let binding = match join_binding(envelope.project_id, target.context_id, result) {
                    Ok(binding) => binding,
                    Err(error) => return result::wrap(Envelope::new(id, project, Err(error))),
                };
                if self
                    .required_context
                    .is_some_and(|id| id != binding.context_id)
                {
                    // A verified native callback owns the replacement slot.
                    // This old immutable slot may return its receipt, not adopt it.
                    state.pending = None;
                    return result::wrap(envelope);
                }
                target.context_id = Some(binding.context_id);
                state.binding = Some(binding.clone());
                state.pending = None;
                let healthy = if let Some(attachment) = &state.attachment {
                    attachment.check().await.is_ok()
                } else {
                    false
                };
                if !healthy {
                    target.expected_project_id = Some(binding.project_id);
                    match self.backend.attach(target).await {
                        Ok(attachment) if attachment.binding() == &binding => {
                            state.attachment = Some(attachment)
                        }
                        outcome => {
                            let mut error = match outcome {
                                Err(error) => error,
                                _ => Error::new(
                                    ErrorCode::IntegrityError,
                                    "Restored identity differs from the committed join.",
                                ),
                            };
                            error.details["request_processed"] = true.into();
                            error.details["request_id"] = id.to_string().into();
                            envelope = Envelope::new(id, Some(binding.project_id), Err(error));
                        }
                    }
                }
            } else if !original_pending
                && matches!(&envelope.outcome,Outcome::Failure{error} if !matches!(error.code,ErrorCode::IoError|ErrorCode::StorageError|ErrorCode::RuntimeUnavailable))
            {
                state.pending = None;
            }
        }
        result::wrap(envelope)
    }
    pub(super) async fn clear(&self) {
        if let Some(routing) = &self.routing
            && let Some(conversations) = routing.conversations.upgrade()
        {
            conversations.close().await;
        }
        self.clear_binding().await;
    }
    pub(super) async fn clear_binding(&self) {
        *self.state.lock().await = State::default();
    }
}
fn validate(state: &State, target: &mut Target, operation: &Operation, id: Uuid) -> Result<()> {
    if let Some(bound) = &state.binding {
        if target
            .context_id
            .is_some_and(|given| given != bound.context_id)
        {
            return Err(identity());
        }
        target.context_id = Some(bound.context_id);
        target.expected_project_id = Some(bound.project_id);
        return Ok(());
    }
    if let Some(pending) = &state.pending {
        if target
            .context_id
            .is_some_and(|given| given != pending.context)
        {
            return Err(identity());
        }
        if let Operation::Mutate {
            command: command @ Mutation::Join { .. },
        } = operation
            && (id != pending.id || command != &pending.command)
        {
            return Err(identity());
        }
    }
    if matches!(
        operation,
        Operation::Query {
            query: agentchat_core::read::Query::Whoami
        }
    ) && target.context_id.is_none()
    {
        return Err(Error::new(
            ErrorCode::SessionRequired,
            "Whoami requires this connection's verified native actor.",
        ));
    }
    match operation {
        Operation::Mutate {
            command: Mutation::Join { .. },
        } if target.context_id.is_some() => Ok(()),
        Operation::Mutate { .. } => Err(Error::new(
            ErrorCode::SessionRequired,
            "Join or explicitly restore this MCP connection before agent mutations.",
        )),
        _ if target.project_path.is_none() => Err(Error::new(
            ErrorCode::InvalidArgument,
            "An unbound MCP operation requires an explicit project_path.",
        )),
        _ => Ok(()),
    }
}
fn join_binding(project: Option<Uuid>, context: Option<Uuid>, result: &Value) -> Result<Binding> {
    let error = || {
        Error::new(
            ErrorCode::IntegrityError,
            "Join result lacks its exact project/context/agent identity.",
        )
    };
    Ok(Binding {
        project_id: project.ok_or_else(error)?,
        context_id: result
            .get("context_id")
            .map(|id| serde_json::from_value(id.clone()).map_err(|_| error()))
            .transpose()?
            .or(context)
            .ok_or_else(error)?,
        agent_id: serde_json::from_value(result["agent_id"].clone()).map_err(|_| error())?,
    })
}
fn identity() -> Error {
    Error::new(
        ErrorCode::IdentityConflict,
        "A live MCP attachment cannot switch context or unresolved join intent.",
    )
}
fn busy() -> Error {
    let mut error = Error::new(
        ErrorCode::Busy,
        "Another operation is updating this MCP connection binding.",
    );
    error.retryable = true;
    error
}
fn retired() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "This host connection has retired; a fresh verified connection is required.",
    )
}
