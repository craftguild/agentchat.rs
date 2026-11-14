//! Independent application checks never run under the native cursor lock.
use super::{Scope, required};
use agentchat_core::{Result, model::Context};
use agentchat_hosts::claude::{
    NativeLifecycle, NativeRegistration,
    body::{InputEntry, InputFile, InputReceipt, NativeInputResult, PendingInput},
    namespace::{CommittedLifecycle, OwnerNamespace},
};
use agentchat_runtime::project::ProjectManager;
use uuid::Uuid;

pub(super) async fn current(
    ns: &OwnerNamespace,
    scope: &Scope,
) -> Result<(CommittedLifecycle, NativeRegistration)> {
    let current = ns.read_current().await?.ok_or_else(required)?;
    let NativeLifecycle::Project(native) = &current.lifecycle else {
        return Err(required());
    };
    if native.root != scope.root
        || native.generation != scope.generation
        || !agentchat_hosts::claude::module::has_startup_gate_source(&native.context.host_version)
        || current.reservation.transport_nonce
            != Some(ns.transport().await?.ok_or_else(required)?.nonce)
    {
        return Err(required());
    }
    Ok((current.clone(), native.clone()))
}
pub(super) async fn load(
    manager: &ProjectManager,
    ns: &OwnerNamespace,
    scope: &Scope,
    token: Option<Uuid>,
) -> Result<Option<InputFile>> {
    let (current, native) = current(ns, scope).await?;
    let directory = ns.directory().to_owned();
    let path = PendingInput::path(&directory, native.context.context_id);
    if !exists(&path)? {
        return Ok(None);
    }
    let pending = PendingInput::read(&path)?;
    let file = pending.input(&directory)?;
    if file.root != native.root
        || file.native_generation != native.generation
        || file.context_id != native.context.context_id
        || file.incarnation != native.context.incarnation
        || token.is_some_and(|token| token != file.token)
    {
        return Err(required());
    }
    if !file.supports_entry_claim() {
        return Ok(None);
    }
    let receipt = InputReceipt::path(&directory, file.context_id, file.token);
    if exists(&receipt)? {
        if !InputReceipt::read(&receipt)?.matches(&file)? {
            return Err(required());
        }
        return Ok(None);
    }
    let entry = InputEntry::path_for(&directory, &file)?;
    if exists(&entry)? {
        if !InputEntry::read(&entry)?.matches(&file)? {
            return Err(required());
        }
        return Ok(None);
    }
    if !app_current(manager, &native.context, &file).await? {
        return Ok(None);
    }
    // A root/token transition during the snapshot invalidates this loaded file.
    // No native getter, writer or application mutation is called under the lock.
    ns.publish_current(current.reservation, move |_| {
        if PendingInput::read(&path)? != pending || pending.input(&directory)? != file {
            return Err(required());
        }
        Ok(Some(file))
    })
    .await
}
// Persist before either native route. A lost helper reply or process crash is
// unknown, so a later load must not expose another native entry for this token.
pub(super) async fn claim(
    manager: &ProjectManager,
    ns: &OwnerNamespace,
    scope: &Scope,
    context: Uuid,
    token: Uuid,
) -> Result<bool> {
    let Some(file) = load(manager, ns, scope, Some(token)).await? else {
        return Ok(false);
    };
    if file.context_id != context {
        return Err(required());
    }
    let (current, _) = current(ns, scope).await?;
    let directory = ns.directory().to_owned();
    let publisher = ns.metadata_publisher()?;
    ns.publish_current(current.reservation, move |_| {
        let pending = PendingInput::read(&PendingInput::path(&directory, context))?;
        if !pending.matches(&file) || pending.input(&directory)? != file {
            return Err(required());
        }
        InputEntry::claim(&file, &publisher, &directory)
    })
    .await
}
/// Validate a prompt-first continuation without inventing an accepted row.
///
/// The exact head must still be current, unreceipted and protected by its
/// already durable one-shot entry intent. This read cannot authorize replay
/// after the application commits a successor or a native result is lost.
pub(super) async fn continuation(
    manager: &ProjectManager,
    ns: &OwnerNamespace,
    scope: &Scope,
    context: Uuid,
    token: Uuid,
) -> Result<InputFile> {
    let (current, native) = current(ns, scope).await?;
    let directory = ns.directory().to_owned();
    let pending_path = PendingInput::path(&directory, context);
    let pending = PendingInput::read(&pending_path)?;
    let file = pending.input(&directory)?;
    let receipt = InputReceipt::path(&directory, context, token);
    let entry = InputEntry::path_for(&directory, &file)?;
    if file.root != native.root
        || file.native_generation != native.generation
        || file.context_id != context
        || file.context_id != native.context.context_id
        || file.incarnation != native.context.incarnation
        || file.token != token
        || !file.supports_entry_claim()
        || !file.input()?.addressed()
        || exists(&receipt)?
        || !InputEntry::read(&entry)?.matches(&file)?
        || !app_current(manager, &native.context, &file).await?
    {
        return Err(required());
    }
    ns.publish_current(current.reservation, move |_| {
        if PendingInput::read(&pending_path)? != pending
            || pending.input(&directory)? != file
            || exists(&receipt)?
            || !InputEntry::read(&entry)?.matches(&file)?
        {
            return Err(required());
        }
        Ok(file)
    })
    .await
}
async fn app_current(
    manager: &ProjectManager,
    context: &Context,
    file: &InputFile,
) -> Result<bool> {
    let project = manager
        .discover(&context.working_directory)
        .await?
        .ok_or_else(required)?;
    if project.identity().id != context.project_id {
        return Err(required());
    }
    let input = file.input()?;
    let paths = agentchat_runtime::daemon::RuntimePaths::for_project(&project);
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let mut client =
            agentchat_protocol::rpc::Client::connect(paths.socket, context.project_id).await?;
        agentchat_protocol::rpc::success(
            client
                .call(
                    Uuid::new_v4(),
                    // Full bodies and lifecycle notices share this head; the
                    // message-only selector cannot authorize its notice item.
                    agentchat_protocol::rpc::Call::CheckNative {
                        context_id: context.context_id,
                        generation: input.generation(),
                        delivery_id: input.delivery_id(),
                        token: file.token,
                    },
                )
                .await?,
        )
    })
    .await
    .map_err(|_| required())??;
    Ok(result["current"] == true)
}
pub(super) async fn record(
    ns: &OwnerNamespace,
    scope: Scope,
    context: Uuid,
    token: Uuid,
    result: NativeInputResult,
) -> Result<()> {
    let (current, native) = current(ns, &scope).await?;
    if native.context.context_id != context {
        return Err(required());
    }
    let directory = ns.directory().to_owned();
    let publisher = ns.metadata_publisher()?;
    ns.publish_current(current.reservation, move |_| {
        let pending = PendingInput::read(&PendingInput::path(&directory, context))?;
        if pending.token != token
            || pending.root != scope.root
            || pending.native_generation != scope.generation
        {
            return Err(required());
        }
        let file = pending.input(&directory)?;
        // Recording describes an already returned native result. Later pause/
        // approval alone cannot downgrade it. Changed root/generation/pointer
        // still fences the callback, including late attempts to recreate a
        // successor's removed transport files. This never authorizes input.
        InputReceipt::from_native(&file, result)?.publish(&publisher, &directory)
    })
    .await
}
pub(super) fn exists(path: &std::path::Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(required()),
    }
}
