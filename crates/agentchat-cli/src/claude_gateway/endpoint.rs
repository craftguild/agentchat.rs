//! Native physical I/O ownership belongs to the host adapter, not CLI policy.
pub(super) use agentchat_hosts::claude::namespace::TransportLease as Lease;
pub(super) async fn bind(
    root: &std::path::Path,
    scope: uuid::Uuid,
    owner: &agentchat_hosts::claude::process::ProcessIdentity,
    namespace: Option<&agentchat_hosts::claude::namespace::OwnerNamespace>,
) -> agentchat_core::Result<Lease> {
    if let Some(namespace) = namespace {
        Lease::bind_managed(namespace).await
    } else {
        Lease::bind(root, scope, owner).await
    }
}
