//! Native callback registration before or after explicit project initialization.
use super::protocol::{self, LifecycleOwner, NativeOwner, NativeProfile, Result, Snapshot};
use agentchat_core::model::HostKind;
use agentchat_hosts::claude::{
    NativeBootstrap, NativeLifecycle, NativeRegistration, process::ProcessIdentity,
};
use agentchat_runtime::{
    context::{ContextRegistry, ContextSeed},
    project::ProjectManager,
};
use std::path::Path;

pub(super) async fn register(root: &Path, native: &Snapshot) -> Result<LifecycleOwner> {
    let path = protocol::registration(root, &native.observer)?;
    let profile: NativeProfile = protocol::read(&root.join("native-profile.json"))?;
    if profile.version != "2.1.301" {
        return Err("unverified native probe version".into());
    }
    let owner = ProcessIdentity::parent(&profile.executable)?;
    if path.exists() {
        let old: LifecycleOwner = protocol::read(&path)?;
        if old.owner != owner
            || old.registration.owner_root() != native.owner_root
            || old.registration.observer() != native.observer
        {
            return Err("native physical owner changed".into());
        }
        if old.registration.generation() == native.generation {
            if old.registration.root() != native.root
                || old.registration.working_directory() != native.cwd.canonicalize()?
                || old.registration.host_version() != profile.version
            {
                return Err("conflicting native generation".into());
            }
            return Ok(old);
        }
        if old.registration.generation() >= native.generation {
            return Err("stale native registration".into());
        }
    }
    let manager = ProjectManager::from_environment()?;
    let directory = native.cwd.canonicalize()?;
    let registration = if manager.discover(&directory).await?.is_some() {
        let context = ContextRegistry::new(manager)
            .issue(ContextSeed {
                host: HostKind::Claude,
                host_session_id: native.root.to_string(),
                host_version: profile.version,
                os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                working_directory: directory,
            })
            .await?;
        NativeLifecycle::Project(NativeRegistration {
            owner_root: native.owner_root,
            observer: native.observer.clone(),
            generation: native.generation,
            root: native.root,
            context,
        })
    } else {
        NativeLifecycle::Bootstrap(NativeBootstrap {
            owner_root: native.owner_root,
            observer: native.observer.clone(),
            generation: native.generation,
            root: native.root,
            lifecycle_id: uuid::Uuid::new_v4(),
            host_version: profile.version,
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: directory,
        })
    };
    let result = LifecycleOwner {
        registration,
        owner,
    };
    if let NativeLifecycle::Project(registration) = &result.registration {
        let initialized = NativeOwner {
            registration: registration.clone(),
            owner: result.owner.clone(),
        };
        let index = root.join(format!("context-{}.json", registration.context.context_id));
        if index.exists() {
            if protocol::read::<NativeOwner>(&index)? != initialized {
                return Err("conflicting immutable native context index".into());
            }
        } else {
            protocol::write(&index, &initialized)?;
        }
        // Existing candidate HostGate readers retain their project-only schema.
        // This locator never gives a bootstrap root presence or wake authority.
        protocol::write(
            &root.join(format!("native-{}.json", native.observer)),
            &initialized,
        )?;
    }
    protocol::write(&path, &result)?;
    protocol::audit(
        root,
        serde_json::json!({"event":"native_registration",
        "observer":native.observer,"root":native.root,"generation":native.generation,
        "scope":format!("{:?}",result.registration.scope()),"owner":result.owner}),
    )?;
    Ok(result)
}

/// Read-only native completion check. It cannot issue or switch a descriptor.
/// A positive result asks the module to run a fresh higher-generation callback.
pub(super) async fn promotion_needed(root: &Path, native: &Snapshot) -> Result<bool> {
    let current: LifecycleOwner = protocol::read(&protocol::registration(root, &native.observer)?)?;
    let profile: NativeProfile = protocol::read(&root.join("native-profile.json"))?;
    if current.owner != ProcessIdentity::parent(&profile.executable)?
        || current.registration.owner_root() != native.owner_root
        || current.registration.root() != native.root
        || current.registration.generation() != native.generation
        || current.registration.working_directory() != native.cwd.canonicalize()?
    {
        return Err("native completion lifecycle changed".into());
    }
    Ok(
        matches!(current.registration, NativeLifecycle::Bootstrap(_))
            && ProjectManager::from_environment()?
                .discover(&native.cwd)
                .await?
                .is_some(),
    )
}
