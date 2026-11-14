//! A separate owner-supervisor request; observation questions never mutate scope.
use super::{
    files,
    identity::Directory,
    protocol::{self, ContextRefresh, Endpoint, Receipt},
    unavailable,
};
use crate::{
    claude::{NativeLifecycle, namespace::OwnerNamespace},
    launch::{Binding, Endpoint as LaunchEndpoint, LaunchOwners, Registry},
};
use agentchat_core::Result;
use std::time::Duration;
use tokio::{io::AsyncWrite, net::UnixStream};

async fn authorized(
    namespace: &OwnerNamespace,
    peer: &UnixStream,
    request: &ContextRefresh,
) -> Result<bool> {
    let Some(pid) = peer
        .peer_cred()
        .ok()
        .and_then(|c| c.pid())
        .and_then(|p| u32::try_from(p).ok())
    else {
        return Ok(false);
    };
    let Some(before) = namespace.read_current().await? else {
        return Ok(false);
    };
    let NativeLifecycle::Project(native) = &before.lifecycle else {
        return Ok(false);
    };
    if native.context.context_id != request.refresh_context
        || native.generation != request.native_generation
    {
        return Ok(false);
    }
    let registry = Registry::new(namespace.state_home());
    let Some(binding) = registry.resolve(&native.context)? else {
        return Ok(false);
    };
    Ok(binding.endpoint == LaunchEndpoint::Claude
        && binding.identity.generation == request.launch_generation
        && LaunchOwners::supervisor_peer(&binding.identity, namespace.owner(), pid)
        && !registry.root_changed(
            native.context.project_id,
            native.context.host,
            &native.context.host_session_id,
            &binding.identity,
        )
        && namespace.read_current().await?.as_ref() == Some(&before))
}
pub(super) async fn emit<W: AsyncWrite + Unpin>(
    namespace: &OwnerNamespace,
    peer: &UnixStream,
    request: &ContextRefresh,
    output: &mut W,
) -> Result<bool> {
    if !authorized(namespace, peer, request).await? {
        return Ok(false);
    }
    // This is a finite registration hint, not a claimed native root or a fresh
    // API snapshot. The native handler recaptures root/marker/epoch itself.
    protocol::write(
        output,
        &serde_json::json!({"event":"context_refresh",
        "context_id":request.refresh_context,"native_generation":request.native_generation}),
    )
    .await
    .map_err(|_| super::output_lost())?;
    Ok(true)
}
pub(crate) async fn request_context_refresh(
    namespace: &OwnerNamespace,
    binding: &Binding,
    native_generation: u64,
) -> Result<()> {
    if binding.endpoint != LaunchEndpoint::Claude
        || !LaunchOwners::is_supervisor(&binding.identity)
        || LaunchOwners::native_owner(&binding.identity)? != *namespace.owner()
    {
        return Err(unavailable());
    }
    // Accept only the original exact relay publication and its live process.
    let root = Directory::open(namespace.directory())?;
    let name = format!("fresh-{}.json", namespace.observer());
    let snapshot = root.read(name.as_ref())?.ok_or_else(unavailable)?;
    let claim =
        super::allocation_publication(&root, namespace.directory(), name.as_ref(), &snapshot)?;
    if claim.native != *namespace.owner() || !claim.helper.is_continuing() {
        return Err(unavailable());
    }
    let endpoint: Endpoint = serde_json::from_slice(&snapshot.bytes).map_err(|_| unavailable())?;
    files::socket(&endpoint.socket).await?;
    let request = ContextRefresh {
        refresh_context: binding.context.context_id,
        native_generation,
        launch_generation: binding.identity.generation,
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut peer = UnixStream::connect(&endpoint.socket)
            .await
            .map_err(|_| unavailable())?;
        if !files::same_user(&peer)
            || peer
                .peer_cred()
                .ok()
                .and_then(|c| c.pid())
                .and_then(|p| u32::try_from(p).ok())
                != Some(claim.helper.pid())
        {
            return Err(unavailable());
        }
        protocol::write(&mut peer, &request).await?;
        let receipt: Receipt = protocol::read(&mut peer, 1024).await?;
        if receipt.nonce != request.refresh_context || !receipt.accepted {
            return Err(unavailable());
        }
        Ok(())
    })
    .await
    .map_err(|_| unavailable())?
}
