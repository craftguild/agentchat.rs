//! Actual gateway, shared production operations and native control integration.
use super::{
    Configuration, Diagnostics,
    projection::Projection,
    protocol::{self, Control, LifecycleOwner as NativeOwner, Request, Response, Result},
    restoration::Restoration,
};
use agentchat_core::{Error, ErrorCode, Result as CoreResult};
use agentchat_hosts::claude::{
    CallProofs, NativeLifecycle, NativeScope, freshness::Freshness, process::ProcessIdentity,
};
use agentchat_mcp::{BootstrapKey, ConversationKey, Conversations};
use agentchat_protocol::{framing, operations::Backend};
use agentchat_runtime::{context::ContextRegistry, project::ProjectManager};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{io::BufReader, net::UnixStream, sync::Mutex};
use uuid::Uuid;

#[path = "managed.rs"]
mod managed;
#[path = "verification.rs"]
mod verification;

struct Bridge {
    physical_owner: ProcessIdentity,
    profile_version: String,
    cursor: Option<agentchat_hosts::claude::namespace::OwnerNamespace>,
    transport_nonce: Uuid,
    os_profile: String,
    native: Mutex<Native>,
    roots: Arc<Conversations>,
    manager: ProjectManager,
    directory: PathBuf,
    diagnostics: Diagnostics,
    restoration: Mutex<Restoration>,
    projection: Mutex<Projection>,
}
struct Native {
    proofs: CallProofs,
    // One immutable real CLI owner per physical gateway. Clear may advance its
    // native root, but another process/helper cannot take over this authority.
    owner: Option<ProcessIdentity>,
    freshness: Freshness,
}
impl Bridge {
    async fn register(&self, input: NativeOwner) -> CoreResult<()> {
        let NativeOwner {
            registration,
            owner,
        } = input;
        let os = match &registration {
            NativeLifecycle::Bootstrap(b) => &b.os_profile,
            NativeLifecycle::Project(p) => &p.context.os_profile,
        };
        if registration.host_version() != self.profile_version || os != &self.os_profile {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "The native lifecycle does not match this gateway's selected native engine/platform.",
            ));
        }
        self.check_cursor(&registration).await?;
        if let NativeLifecycle::Project(project) = &registration {
            let context = ContextRegistry::new(self.manager.clone())
                .resolve(project.context.context_id, None)
                .await?;
            if context != project.context {
                return Err(Error::new(
                    ErrorCode::IdentityConflict,
                    "Native descriptor changed.",
                ));
            }
        }
        let scope = registration.scope();
        let root = registration.root();
        let pending_launch = self.pending_launch(&registration)?;
        let continuation = {
            let mut native = self.native.lock().await;
            if owner != self.physical_owner
                || !owner.is_continuing()
                || native.owner.as_ref().is_some_and(|old| old != &owner)
            {
                return Err(Error::new(
                    ErrorCode::SessionRequired,
                    "Native physical owner changed.",
                ));
            }
            let continuation = matches!((native.proofs.lifecycle(),&registration),
                (Some(NativeLifecycle::Project(old)),NativeLifecycle::Project(new)) if old.root==new.root);
            native.proofs.register_lifecycle(registration)?;
            native.owner = Some(owner);
            continuation
        };
        match scope {
            NativeScope::Bootstrap {
                lifecycle_id,
                generation,
            } => {
                self.roots
                    .activate_bootstrap(
                        BootstrapKey {
                            lifecycle_id,
                            generation,
                        },
                        agentchat_protocol::operations::InitializationIdentity::new(
                            agentchat_core::model::HostKind::Claude,
                            root.to_string(),
                        )?,
                    )
                    .await?;
            }
            NativeScope::Project(scope) => {
                let key = ConversationKey {
                    context_id: scope.context_id,
                    generation: scope.generation,
                };
                let conversation = if continuation {
                    self.roots.activate_same_conversation(key).await?
                } else {
                    self.roots.activate(key).await?
                };
                // SessionStart must return before fresh native getters can run.
                // The existing one-owned-task readiness/restoration path is kept.
                self.restoration
                    .lock()
                    .await
                    .start(conversation, self.diagnostics.clone(), pending_launch)
                    .await;
            }
        }
        Ok(())
    }
    async fn apply(&self, control: Control) -> CoreResult<()> {
        match control {
            Control::Register { registration } => self.register(*registration).await?,
            Control::Record { registration, call } => {
                self.register(*registration).await?;
                self.native.lock().await.proofs.record(call)?;
            }
            Control::Revoke {
                observer,
                generation,
                root,
                tool_use_id,
            } => {
                self.native
                    .lock()
                    .await
                    .proofs
                    .revoke(&observer, generation, root, &tool_use_id);
            }
            Control::Retire {
                observer,
                generation,
                root,
            } => {
                let scope = self
                    .native
                    .lock()
                    .await
                    .proofs
                    .retire_scope(&observer, generation, root);
                if let Some(scope) = scope {
                    match scope {
                        NativeScope::Project(scope) => {
                            let key = ConversationKey {
                                context_id: scope.context_id,
                                generation: scope.generation,
                            };
                            self.roots.retire(key).await?;
                            self.restoration.lock().await.retire(key).await;
                        }
                        NativeScope::Bootstrap {
                            lifecycle_id,
                            generation,
                        } => {
                            self.roots
                                .retire_bootstrap(BootstrapKey {
                                    lifecycle_id,
                                    generation,
                                })
                                .await?;
                        }
                    }
                }
            }
            Control::Refresh {
                observer,
                generation,
                root,
            } => {
                let native = self.native.lock().await;
                if matches!(
                    native.proofs.lifecycle(),
                    Some(NativeLifecycle::Bootstrap(_))
                ) {
                    return Ok(());
                }
                let current = native
                    .proofs
                    .current()
                    .filter(|r| {
                        r.observer == observer
                            && r.generation == generation
                            && r.root == root
                            && native
                                .owner
                                .as_ref()
                                .is_some_and(|owner| owner.is_continuing())
                    })
                    .ok_or_else(|| {
                        Error::new(
                            ErrorCode::SessionRequired,
                            "Native change hint is obsolete.",
                        )
                    })?;
                // Never wait on a native getter while its callback waits for
                // this control reply. The single coalescing worker performs
                // the existing verified heartbeat after callback progress.
                self.projection.lock().await.request(ConversationKey {
                    context_id: current.context.context_id,
                    generation,
                });
            }
        };
        Ok(())
    }
    async fn control(&self, stream: UnixStream, nonce: Uuid) -> Result<()> {
        let (read, mut write) = stream.into_split();
        let request: Request = framing::read(&mut BufReader::new(read))
            .await?
            .ok_or("control EOF")?;
        let result = if request.nonce == nonce {
            self.apply(request.control).await
        } else {
            Err(Error::new(
                ErrorCode::SessionRequired,
                "Stale control endpoint.",
            ))
        };
        framing::write(
            &mut write,
            &Response {
                accepted: result.is_ok(),
                error: result.err().map(|e| e.code),
            },
        )
        .await?;
        Ok(())
    }
}
pub(super) async fn run(
    config: Configuration,
    backend: Arc<dyn Backend>,
    manager: ProjectManager,
    diagnostics: Diagnostics,
) -> Result<()> {
    let root = &config.directory;
    protocol::private_directory(root)?;
    let physical_owner = if let Some(cursor) = &config.owner_cursor {
        let owner = cursor.owner().clone();
        owner.verify_ancestor()?;
        owner
    } else {
        ProcessIdentity::ancestor(&config.profile.executable)?
    };
    if let Some(cursor) = &config.owner_cursor
        && (cursor.directory() != root
            || cursor.id() != config.owner_hint
            || cursor.owner() != &physical_owner)
    {
        return Err("Native cursor does not belong to this physical gateway".into());
    }
    let lease = super::endpoint::bind(
        root,
        config.owner_hint,
        &physical_owner,
        config.owner_cursor.as_ref(),
    )
    .await?;
    let nonce = lease.nonce;
    let listener = lease.listener.clone();
    let roots = Arc::new(Conversations::new(backend));
    let bridge = Arc::new(Bridge {
        physical_owner: physical_owner.clone(),
        cursor: config.owner_cursor,
        transport_nonce: nonce,
        profile_version: config.profile.version.clone(),
        os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        native: Mutex::new(Native {
            proofs: CallProofs::new(config.owner_hint, config.tool_prefix)?,
            owner: None,
            freshness: Freshness::default(),
        }),
        roots: roots.clone(),
        manager,
        directory: root.clone(),
        diagnostics: diagnostics.clone(),
        restoration: Mutex::default(),
        projection: Mutex::new(Projection::new(&roots, diagnostics.clone())),
    });
    // Installed composition waits for an authoritative callback against this
    // fresh endpoint nonce. A historical row may carry an expired incarnation;
    // never replay it on transport restart. The isolated feasibility engine
    // retains its separately proved legacy startup handshake.
    let pending = protocol::current_owner(root, config.owner_hint);
    if bridge.cursor.is_none() && pending.exists() {
        let native: NativeOwner = protocol::read(&pending)?;
        if native.registration.owner_root() != config.owner_hint
            || native.owner != bridge.physical_owner
            || !native.owner.is_continuing()
        {
            return Err("Native startup owner changed".into());
        }
        bridge.register(native).await?;
    }
    let controller = bridge.clone();
    let mut maintenance = super::maintenance::Maintenance::new(bridge.manager.home());

    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            if stream.peer_cred().map(|p| p.uid()).ok() != Some(rustix::process::geteuid().as_raw())
            {
                continue;
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), controller.control(stream, nonce))
                .await;
        }
    });
    diagnostics.record(serde_json::json!({"event":"gateway_start","owner_hint":config.owner_hint,"pid":std::process::id()}));
    let serving = agentchat_mcp::stdio(agentchat_mcp::Server::gateway(
        roots.clone(),
        bridge.clone(),
    ));
    tokio::pin!(serving);
    let mut ticks = tokio::time::interval(Duration::from_millis(250));
    let result = loop {
        tokio::select! {
            result = &mut serving => break result,
            _ = ticks.tick() => {
                match physical_owner.continuity_state() {
                    agentchat_hosts::claude::process::ContinuityState::Gone => break Ok(()),
                    agentchat_hosts::claude::process::ContinuityState::Unknown => continue,
                    agentchat_hosts::claude::process::ContinuityState::Current => {}
                }
                maintenance.tick().await;
            },
        }
    };
    // Parent loss must close owned attachments even if another process keeps
    // a copy of stdin open. Cleanup also fences late control/verifier results.
    task.abort();
    let _ = task.await;
    roots.close().await;
    bridge.restoration.lock().await.close().await;
    bridge.projection.lock().await.close().await;
    {
        let mut native = bridge.native.lock().await;
        native.proofs.close();
        native.freshness.close();
    }
    diagnostics.record(serde_json::json!({"event":"gateway_exit","owner_hint":config.owner_hint,"exit_ok":result.is_ok()}));
    result?;
    Ok(())
}
