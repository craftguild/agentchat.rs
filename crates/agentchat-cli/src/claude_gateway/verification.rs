//! Fresh per-call native proof; no spawn hint or project discovery fallback.
use super::Bridge;
use agentchat_core::{Error, ErrorCode, Result as CoreResult};
use agentchat_hosts::claude::NativeScope;
use agentchat_mcp::{BootstrapKey, CallEvidence, CallScope, CallVerifier, ConversationKey};
use async_trait::async_trait;
use std::time::Duration;
#[cfg(test)]
#[path = "../../tests/unit/gateway_verification.rs"]
mod tests;

#[async_trait]
impl CallVerifier for Bridge {
    async fn verify(&self, evidence: CallEvidence<'_>) -> CoreResult<ConversationKey> {
        match self.verify_scope(evidence).await? {
            CallScope::Conversation(key) => Ok(key),
            CallScope::Bootstrap(_) => Err(Error::new(
                ErrorCode::SessionRequired,
                "No project context exists.",
            )),
        }
    }
    async fn verify_scope(&self, evidence: CallEvidence<'_>) -> CoreResult<CallScope> {
        let id = evidence
            .metadata
            .and_then(|m| m.get("claudecode/toolUseId"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::SessionRequired,
                    "Exact native metadata is required.",
                )
            })?;
        let (scope, registration, owner, challenge) = {
            // Share the existing five-second verification budget, before
            // consuming the one-use proof. Native control holds this lock only
            // for bounded state updates; the transport caps waiting calls at 32.
            // Validate the current owner and lifecycle after the wait, never
            // facts captured before a concurrent retirement or root change.
            let mut native = tokio::time::timeout(Duration::from_secs(5), self.native.lock())
                .await
                .map_err(|_| {
                    let mut error =
                        Error::new(ErrorCode::Busy, "Native control verification timed out.");
                    error.retryable = true;
                    error
                })?;
            let owner = native
                .owner
                .as_ref()
                .filter(|owner| owner.is_continuing())
                .cloned()
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::SessionRequired,
                        "Exact native owner is unavailable.",
                    )
                })?;
            let scope = native.proofs.consume_scope(evidence.tool_name, id)?;
            let registration =
                native.proofs.lifecycle().cloned().ok_or_else(|| {
                    Error::new(ErrorCode::SessionRequired, "Native root retired.")
                })?;
            let challenge =
                native
                    .freshness
                    .begin_lifecycle(&registration, &owner, Duration::from_secs(5))?;
            (scope, registration, owner, challenge)
        };
        if let Err(error) = self.check_cursor(&registration).await {
            self.native.lock().await.freshness.cancel(challenge.nonce);
            return Err(error);
        }
        // Native control must remain able to retire/switch roots while the
        // observer round trip waits. No lifecycle lock spans external I/O.
        let answer = match agentchat_hosts::claude::observer::exchange(
            &self.directory,
            registration.observer(),
            owner.pid(),
            &challenge,
        )
        .await
        {
            Ok(answer) => answer,
            Err(_) => {
                self.native.lock().await.freshness.cancel(challenge.nonce);
                return Err(Error::new(
                    ErrorCode::SessionRequired,
                    "Fresh native observation is unavailable.",
                ));
            }
        };
        if let Err(error) = self.check_cursor(&registration).await {
            self.native.lock().await.freshness.cancel(challenge.nonce);
            return Err(error);
        }
        let native_sequence = answer.native_sequence;
        let pause_cause = answer.pause_cause.clone();
        let state =
            {
                let mut native = self.native.lock().await;
                let current = native.proofs.lifecycle().cloned().ok_or_else(|| {
                    Error::new(ErrorCode::SessionRequired, "Native root retired.")
                })?;
                let current_owner = native.owner.clone().ok_or_else(|| {
                    Error::new(ErrorCode::SessionRequired, "Native owner retired.")
                })?;
                native
                    .freshness
                    .finish_lifecycle(&current, &current_owner, answer)?
            };
        // Ordinary host-approved tool routing is separate from wake policy.
        // The permission latch can remain set until this very tool completes;
        // its phase is not permission to bypass the host's approval decision.
        let root = registration.root();
        let owner_pid = owner.pid();
        // Optional diagnostics cannot fail or grant an otherwise valid call.
        self.diagnostics.record(serde_json::json!({"event":"native_call_consumed","root":root,"scope":format!("{scope:?}"),"generation":registration.generation(),"tool":evidence.tool_name,"tool_use_id":id,"owner_pid":owner_pid,"fresh_nonce":challenge.nonce,"native_sequence":native_sequence,"phase":state.phase,"pause_cause":pause_cause}));
        Ok(match scope {
            NativeScope::Project(scope) => CallScope::Conversation(ConversationKey {
                context_id: scope.context_id,
                generation: scope.generation,
            }),
            NativeScope::Bootstrap {
                lifecycle_id,
                generation,
            } => CallScope::Bootstrap(BootstrapKey {
                lifecycle_id,
                generation,
            }),
        })
    }
}
