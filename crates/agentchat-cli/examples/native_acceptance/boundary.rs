//! Bounded native-only callback observation; never joins or submits a prompt.
use super::Result;
use agentchat_hosts::claude::{
    NativeLifecycle, adapter::NativeAdapter, metadata, namespace::OwnerNamespace,
    process::ProcessIdentity,
};
use serde_json::json;
use std::{path::PathBuf, time::Instant};
use uuid::Uuid;

pub(super) async fn observe(
    home: PathBuf,
    native: PathBuf,
    report: PathBuf,
    root: Uuid,
    check_membership: bool,
) -> Result<()> {
    let owner = ProcessIdentity::parent(&native.canonicalize()?)?;
    let started = Instant::now();
    let ns = OwnerNamespace::existing(&home, owner.clone()).await?;
    let before = ns.current().await?.ok_or("No committed native callback")?;
    let NativeLifecycle::Project(registration) = &before.lifecycle else {
        return Err("Uninitialized native root".into());
    };
    if registration.root != root || registration.context.host_session_id != root.to_string() {
        return Err("Native callback and selected root differ".into());
    }
    let state = NativeAdapter::new(home.clone())
        .observe_owned(&registration.context, &owner)
        .await?;
    if ns.current().await?.as_ref() != Some(&before) || !owner.is_current() {
        return Err("Native callback changed during challenge".into());
    }
    let membership = if check_membership {
        let manager = agentchat_runtime::project::ProjectManager::new(&home)?;
        let project = manager
            .discover(&registration.context.working_directory)
            .await?
            .ok_or("Initialized callback project missing")?;
        if project.identity().id != registration.context.project_id {
            return Err("Callback project mismatch".into());
        }
        let paths = agentchat_runtime::daemon::RuntimePaths::for_project(&project);
        let mut client =
            agentchat_protocol::rpc::Client::connect(&paths.socket, project.identity().id).await?;
        let result = client
            .native_binding(registration.context.context_id)
            .await?;
        if result.agent.as_ref().is_none_or(|a| !a.agent.joined) {
            return Err("Membership was not committed before native callback continuation".into());
        }
        Some(result)
    } else {
        None
    };
    let value = json!({"callback_held":true,"owner":owner,"root":root,"membership_at_callback":membership,
        "context":registration.context,"native_generation":registration.generation,
        "phase":state.phase,"wake_paused":state.wake_paused,
        "inherited_probe_locator":std::env::var("AGENTCHAT_STARTUP_PROBE_FLAG").ok(),
        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "membership_created_by_probe":false,"membership_observed":check_membership,"admitted":false});
    metadata::write(&report, &value)?;
    println!("{value}");
    Ok(())
}
