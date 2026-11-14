//! Registry work is outside the cursor lock; projection publication is fenced.
use super::{
    input::Current,
    transport::{self, required},
};
use crate::claude_gateway::records::{self, Control, LifecycleOwner};
use agentchat_core::{Result, model::HostKind};
use agentchat_hosts::claude::{
    NativeBootstrap, NativeLifecycle, NativeRegistration,
    namespace::{CommittedLifecycle, OwnerNamespace, Reservation, remove_notices},
    observer::RegisteredOwner,
};
use agentchat_runtime::{
    context::{ContextRegistry, ContextSeed},
    project::ProjectManager,
};
use uuid::Uuid;

pub(super) fn root_changed(
    manager: &ProjectManager,
    prior: Option<CommittedLifecycle>,
    new_root: Uuid,
) -> Result<()> {
    let Some(prior) = prior else {
        return Ok(());
    };
    let NativeLifecycle::Project(native) = prior.lifecycle else {
        return Ok(());
    };
    if native.root == new_root {
        return Ok(());
    }
    let registry = agentchat_hosts::launch::Registry::new(manager.home());
    if let Some(binding) = registry.resolve(&native.context)? {
        registry.record_root_change(&binding, new_root)?;
    }
    Ok(())
}

pub(super) async fn register(
    manager: &ProjectManager,
    ns: &OwnerNamespace,
    intent: Reservation,
    version: &str,
) -> Result<LifecycleOwner> {
    let prior = ns.registration(intent.clone()).await?;
    if let Some(expected) = intent.transport_nonce
        && transport::endpoint(ns).await?.map(|e| e.nonce) != Some(expected)
    {
        return Err(required());
    }
    let lifecycle = match prior {
        Some(lifecycle) if lifecycle.host_version() == version => lifecycle,
        Some(_) => return Err(required()),
        None => {
            let directory = tokio::fs::canonicalize(&intent.working_directory)
                .await
                .map_err(|_| required())?;
            if directory != intent.working_directory {
                return Err(required());
            }
            let os = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
            let lifecycle = if manager.discover(&directory).await?.is_some() {
                let registry = ContextRegistry::new(manager.clone());
                let proposed = registry
                    .prepare(ContextSeed {
                        host: HostKind::Claude,
                        host_session_id: intent.root.to_string(),
                        host_version: version.into(),
                        os_profile: os,
                        working_directory: directory,
                    })
                    .await?;
                let context = ns.allocate_context(intent.clone(), proposed).await?;
                let context = registry.publish(context).await?;
                NativeLifecycle::Project(NativeRegistration {
                    owner_root: ns.id(),
                    observer: ns.observer().into(),
                    generation: intent.generation,
                    root: intent.root,
                    context,
                })
            } else {
                NativeLifecycle::Bootstrap(NativeBootstrap {
                    owner_root: ns.id(),
                    observer: ns.observer().into(),
                    generation: intent.generation,
                    root: intent.root,
                    lifecycle_id: Uuid::new_v4(),
                    host_version: version.into(),
                    os_profile: os,
                    working_directory: directory,
                })
            };
            // Another callback may supersede this async issue. Its orphan descriptor
            // grants no participation and must never be published as current.
            ns.commit(intent.clone(), lifecycle.clone()).await?;
            lifecycle
        }
    };
    let registration = LifecycleOwner {
        registration: lifecycle,
        owner: ns.owner().clone(),
    };
    let path = ns.directory().to_owned();
    let owner = registration.owner.clone();
    let id = ns.id();
    let home = manager.home().to_owned();
    let publisher = ns.metadata_publisher()?;
    ns.publish_current(intent, move |committed| {
        let registration = LifecycleOwner {
            registration: committed.lifecycle,
            owner,
        };
        publish(&publisher, &path, id, &registration)?;
        if let NativeLifecycle::Project(scope) = &registration.registration {
            agentchat_hosts::claude::routing::publish(
                &home,
                &RegisteredOwner {
                    registration: scope.clone(),
                    owner: registration.owner.clone(),
                },
            )?;
        }
        Ok(())
    })
    .await?;
    transport::send(
        ns,
        Control::Register {
            registration: Box::new(registration.clone()),
        },
        true,
    )
    .await?;
    Ok(registration)
}
fn publish(
    publisher: &agentchat_hosts::claude::metadata::Publisher,
    path: &std::path::Path,
    id: Uuid,
    native: &LifecycleOwner,
) -> Result<()> {
    if let NativeLifecycle::Project(registration) = &native.registration {
        let initialized = RegisteredOwner {
            registration: registration.clone(),
            owner: native.owner.clone(),
        };
        let index = path.join(format!("context-{}.json", registration.context.context_id));
        if index.try_exists().map_err(|_| required())? {
            if records::read::<RegisteredOwner>(&index).map_err(|_| required())? != initialized {
                return Err(required());
            }
        } else {
            publisher
                .write(&index, &initialized)
                .map_err(|_| required())?;
        }
        publisher
            .write(
                &path.join(format!("native-{}.json", registration.observer)),
                &initialized,
            )
            .map_err(|_| required())?;
    }
    publisher
        .write(&records::current_owner(path, id), native)
        .map_err(|_| required())?;
    std::fs::File::open(path)
        .and_then(|d| d.sync_all())
        .map_err(|_| required())?;
    Ok(())
}
pub(super) async fn needed(
    manager: &ProjectManager,
    ns: &OwnerNamespace,
    input: Current,
) -> Result<bool> {
    let Some(current) = ns.current().await? else {
        return Ok(true);
    };
    let cwd = tokio::fs::canonicalize(input.cwd)
        .await
        .map_err(|_| required())?;
    if current.lifecycle.host_version() != input.version
        || current.lifecycle.root() != input.root
        || current.lifecycle.generation() != input.generation
        || current.lifecycle.working_directory() != cwd
        || current.reservation.transport_nonce != transport::endpoint(ns).await?.map(|e| e.nonce)
    {
        return Ok(true);
    }
    Ok(matches!(current.lifecycle, NativeLifecycle::Bootstrap(_))
        && manager.discover(&cwd).await?.is_some())
}
pub(super) async fn current(
    ns: &OwnerNamespace,
    root: Uuid,
    generation: u64,
) -> Result<LifecycleOwner> {
    let current = ns.current().await?.ok_or_else(required)?;
    if current.lifecycle.root() != root
        || current.lifecycle.generation() != generation
        || current.reservation.transport_nonce
            != Some(transport::endpoint(ns).await?.ok_or_else(required)?.nonce)
    {
        return Err(required());
    }
    Ok(LifecycleOwner {
        registration: current.lifecycle,
        owner: ns.owner().clone(),
    })
}
pub(super) fn remove_projection(
    path: &std::path::Path,
    id: Uuid,
    c: &CommittedLifecycle,
    owner: &agentchat_hosts::claude::process::ProcessIdentity,
) -> Result<()> {
    // Keep the registration projection until both notices are gone. A killed
    // retirement then leaves enough exact facts for later dead-owner cleanup.
    if let NativeLifecycle::Project(registration) = &c.lifecycle {
        remove_notices(path, registration)?;
        agentchat_hosts::claude::body::retire_scope(path, registration)?;
    }
    let expected = LifecycleOwner {
        registration: c.lifecycle.clone(),
        owner: owner.clone(),
    };
    records::remove_if(&records::current_owner(path, id), &expected).map_err(|_| required())?;
    if let NativeLifecycle::Project(registration) = &c.lifecycle {
        let expected = RegisteredOwner {
            registration: registration.clone(),
            owner: owner.clone(),
        };
        for name in [
            format!("native-{}.json", registration.observer),
            format!("context-{}.json", registration.context.context_id),
        ] {
            records::remove_if(&path.join(name), &expected).map_err(|_| required())?;
        }
    }
    Ok(())
}
