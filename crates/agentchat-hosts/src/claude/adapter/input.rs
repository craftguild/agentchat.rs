//! One current body publication; polling never re-emits a native input/signal.
use super::{NativeAdapter, required};
use crate::claude::{
    body::{self, InputFile, InputHead, InputReceipt, PendingInput},
    routing, signal,
};
use agentchat_core::{
    Error, ErrorCode, Result,
    delivery::{
        HostReceipt, MessageDispatch, MessagePort, MessageProgress, NativeDispatch, NoticeDispatch,
    },
    model::{Context, Phase},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

enum Preparation {
    Complete(MessageProgress),
    Publish(Box<routing::Route>, InputFile),
}

#[async_trait::async_trait]
impl MessagePort for NativeAdapter {
    async fn present_message(
        &self,
        context: &Context,
        dispatch: &MessageDispatch,
    ) -> Result<MessageProgress> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.input(context, &NativeDispatch::Message(dispatch.clone())),
        )
        .await
        .map_err(|_| required())?
    }

    async fn present_notice(
        &self,
        context: &Context,
        dispatch: &NoticeDispatch,
    ) -> Result<MessageProgress> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.input(context, &NativeDispatch::Notice(dispatch.clone())),
        )
        .await
        .map_err(|_| required())?
    }
}
impl NativeAdapter {
    async fn input(&self, context: &Context, dispatch: &NativeDispatch) -> Result<MessageProgress> {
        if dispatch.awaiting_acceptance() {
            return self.original_receipt(context, dispatch).await;
        }
        let (route, file) = match self.prepare_input(context, dispatch).await {
            Ok(Preparation::Complete(progress)) => return Ok(progress),
            Ok(Preparation::Publish(route, file)) => (route, file),
            Err(error) => return Ok(MessageProgress::NotAccepted(error)),
        };
        let directory = route.namespace.directory().to_owned();
        let ns = route.namespace.clone();
        let publisher = match ns.metadata_publisher() {
            Ok(publisher) => publisher,
            Err(error) => return Ok(MessageProgress::NotAccepted(error)),
        };
        let entered = Arc::new(AtomicBool::new(false));
        let publishing = entered.clone();
        let home = self.home.clone();
        let cwd = context.working_directory.clone();
        let result = ns
            .publish_current(route.reservation(), move |current| {
                // Once this closure can publish, an I/O error is ambiguous. A
                // stale reservation rejected before entry has emitted no input.
                publishing.store(true, Ordering::Release);
                let head = InputHead::new(&file)?;
                if let Some(prior) = InputHead::read(&directory)?
                    && prior != head
                {
                    // One physical owner retains at most one original body and
                    // result, including across root/connection generations.
                    prior.remove_transport(&directory)?;
                }
                let pending_path = PendingInput::path(&directory, file.context_id);
                if body::exists(&pending_path)? {
                    let pending = PendingInput::read(&pending_path)?;
                    if pending.matches(&file) {
                        if pending.input(&directory)? != file {
                            return Err(required());
                        }
                        file.publish(&publisher, &directory)?;
                        head.publish(&publisher, &directory)?;
                        return Ok(());
                    }
                    // AGENTCHAT has committed this successor/retry head before I/O.
                    // Retire only the exact old scope's proved transport files;
                    // no database cursor, signal counter or shared body changes.
                    let crate::claude::NativeLifecycle::Project(scope) = current.lifecycle else {
                        return Err(required());
                    };
                    body::remove_scope(&directory, &scope)?;
                }
                file.publish(&publisher, &directory)?;
                head.publish(&publisher, &directory)?;
                signal::send(&home, &cwd, |counter| {
                    PendingInput::new(&file, counter)?.publish(&publisher, &directory)
                })
            })
            .await;
        if let Err(error) = result {
            return if entered.load(Ordering::Acquire) {
                Err(error)
            } else {
                Ok(MessageProgress::NotAccepted(error))
            };
        }
        Ok(MessageProgress::AwaitingAcceptance)
    }
    // Fresh dispatches are unique application claims. Errors here precede
    // publication; recovered/published attempts use only original_receipt.
    async fn prepare_input(
        &self,
        context: &Context,
        dispatch: &NativeDispatch,
    ) -> Result<Preparation> {
        let route = routing::resolve(&self.home, context).await?;
        if !crate::claude::module::has_startup_gate_source(&route.profile.version) {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "This native full-input consumer version is unsupported.",
            ));
        }
        let file = InputFile::new_native(
            context,
            route.native.registration.root,
            route.native.registration.generation,
            dispatch,
        )?;
        let directory = route.namespace.directory().to_owned();
        let expected = file.clone();
        let path = directory.clone();
        let receipt = tokio::task::spawn_blocking(move || recover(&path, &expected, false))
            .await
            .map_err(|_| required())??;
        if let Some(progress) = receipt {
            // A durable known native success predates any later pause. Reading
            // it performs no native I/O and must not downgrade it from controls.
            return Ok(Preparation::Complete(progress));
        }
        let state = self.observe_route(&route).await?;
        if state.wake_paused || !matches!(state.phase, Phase::Idle | Phase::Active) {
            return Err(required());
        }
        Ok(Preparation::Publish(Box::new(route), file))
    }
    async fn original_receipt(
        &self,
        context: &Context,
        dispatch: &NativeDispatch,
    ) -> Result<MessageProgress> {
        use crate::{
            claude::{NativeLifecycle, namespace::OwnerNamespace},
            launch::{Endpoint, LaunchOwners, Registry},
        };
        use agentchat_core::model::LaunchOwnerState;
        if !crate::claude::module::has_startup_gate_source(&context.host_version) {
            return Err(required());
        }
        let registry = Registry::new(&self.home);
        let binding = registry.resolve(context)?.ok_or_else(required)?;
        if binding.endpoint != Endpoint::Claude
            || binding.identity.generation != dispatch.generation()
            || context.project_id != dispatch.input().project_id()
            || LaunchOwners::observe(&binding.identity) != LaunchOwnerState::Current
            || registry.root_changed(
                context.project_id,
                context.host,
                &context.host_session_id,
                &binding.identity,
            )
        {
            return Err(required());
        }
        let owner = LaunchOwners::native_owner(&binding.identity)?;
        let ns = OwnerNamespace::existing(&self.home, owner).await?;
        let directory = ns.directory().to_owned();
        let expected = dispatch.clone();
        let root = context.host_session_id.clone();
        let (file, receipt) =
            tokio::task::spawn_blocking(move || original_result(&directory, &root, &expected))
                .await
                .map_err(|_| required())??;
        // Known original success is read-only and predates any current native
        // context/control transition. It must not depend on routing that the
        // very transition is retiring. Core rechecks owner/root/head at commit.
        if let Some(receipt) = receipt {
            return receipt_progress(receipt);
        }
        let current = ns.read_current().await?.ok_or_else(required)?;
        if let NativeLifecycle::Project(native) = current.lifecycle
            && native.context == *context
            && native.root == file.root
            && native.generation == file.native_generation
            && file.context_id == context.context_id
            && file.incarnation == context.incarnation
        {
            return Ok(MessageProgress::AwaitingAcceptance);
        }
        // Without a known result, an obsolete scope is unknown rather than a
        // native success or a permit for output from this receipt-only lane.
        Err(required())
    }
}
// Called only after the original managed owner/root/generation check above.
// No path here publishes, rewrites or re-enters the saved original frame.
fn original_result(
    directory: &std::path::Path,
    root: &str,
    expected: &NativeDispatch,
) -> Result<(InputFile, Option<InputReceipt>)> {
    if !expected.awaiting_acceptance() {
        return Err(required());
    }
    let revision = expected.frame_revision();
    let head = InputHead::read(directory)?.ok_or_else(required)?;
    let file = head.original_input(directory, revision)?;
    if file.root.to_string() != root
        || file.token != expected.token()
        || file.original_input(revision)? != expected.input()
    {
        return Err(required());
    }
    let receipt = head.original_receipt(directory, &file, revision)?;
    Ok((file, receipt))
}
fn receipt_progress(receipt: InputReceipt) -> Result<MessageProgress> {
    if receipt.accepted {
        Ok(MessageProgress::Accepted(HostReceipt { turn_id: None }))
    } else if receipt.rejected_before_entry() {
        Ok(MessageProgress::NotAccepted(Error::new(
            ErrorCode::IoError,
            "The native host refused the complete input.",
        )))
    } else {
        Ok(MessageProgress::AwaitingAcceptance)
    }
}

// This lane reads only original metadata. A new context may recognize an old
// success, but cannot send its body or turn a missing/unknown result into success.
fn recover(
    directory: &std::path::Path,
    expected: &InputFile,
    poll: bool,
) -> Result<Option<MessageProgress>> {
    let Some(head) = InputHead::read(directory)? else {
        return Ok(None);
    };
    if !head.same_attempt(expected)? || (!poll && !head.matches(expected)?) {
        return Ok(None);
    }
    let original = head.input(directory)?;
    if original.root != expected.root
        || original.token != expected.token
        || original.input()? != expected.input()?
    {
        return Ok(None);
    }
    if let Some(receipt) = head.receipt(directory, &original)? {
        return receipt_progress(receipt).map(Some);
    }
    if poll && original == *expected {
        return Ok(Some(MessageProgress::AwaitingAcceptance));
    }
    Ok(None)
}

#[cfg(test)]
#[path = "../../../tests/unit/native_input_recovery.rs"]
mod tests;
