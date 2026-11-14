//! One real native round trip; no cached heartbeat or host-admission override.
use super::protocol::{self, LifecycleOwner, NativeOwner, Result};
use agentchat_hosts::claude::{freshness::Freshness, observer::exchange};
use std::{path::Path, time::Duration};

pub(super) async fn state(root: &Path, id: uuid::Uuid) -> Result<()> {
    let owner: NativeOwner = protocol::read(&root.join(format!("context-{id}.json")))?;
    if owner.registration.context.context_id != id {
        return Err("conflicting native context index".into());
    }
    let profile = protocol::read(&root.join("native-profile.json"))?;
    let observer =
        agentchat_hosts::claude::observer::NativeObserver::open(root.into(), profile).await?;
    let state = observer.verify(&owner.registration.context).await?;
    println!(
        "{}",
        serde_json::json!({"root":owner.registration.root,"context_id":id,"phase":state.phase,"active_turn_id":state.active_turn_id,"wake_paused":state.wake_paused})
    );
    Ok(())
}

pub(super) async fn run(root: &Path, observer: &str) -> Result<()> {
    let path = protocol::registration(root, observer)?;
    let native: LifecycleOwner = protocol::read(&path)?;
    let mut proof = Freshness::default();
    let request =
        proof.begin_lifecycle(&native.registration, &native.owner, Duration::from_secs(5))?;
    let answer = exchange(root, observer, native.owner.pid(), &request).await?;
    // Re-read authoritative lifecycle facts after waiting. Clear cannot lend
    // the old descriptor a fresh-looking answer from its replacement root.
    let current: LifecycleOwner = protocol::read(&path)?;
    let state = proof.finish_lifecycle(&current.registration, &current.owner, answer.clone())?;
    println!(
        "{}",
        serde_json::json!({"root":current.registration.root(),"scope":format!("{:?}",current.registration.scope()),
        "phase":state.phase,"active_turn_id":state.active_turn_id,"wake_paused":state.wake_paused,"snapshot":answer})
    );
    Ok(())
}
