//! Real kernel owner/supervisor loss and conservative token observation.
use agentchat_core::model::LaunchOwnerState;
use agentchat_hosts::launch::LaunchOwners;
use std::{
    process::{Child, Command},
    time::{Duration, Instant},
};

struct Owned(Child);
impl Owned {
    fn sleep() -> Self {
        let child = Self(Command::new("/bin/sleep").arg("60").spawn().unwrap());
        let executable = std::fs::canonicalize("/bin/sleep").unwrap();
        let metadata = std::fs::metadata(&executable).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Spawn may return before exec replaces the inherited test image. Keep
        // the intended image stable before testing native ownership fences.
        while !agentchat_hosts::claude::process::ProcessIdentity::capture(child.0.id()).is_ok_and(
            |owner| owner.matches_executable(&executable, metadata.clone()) && owner.is_current(),
        ) {
            assert!(
                std::time::Instant::now() < deadline,
                "Owned sleep image did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        child
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
    fn kill(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn capture(owner: u32, supervisor: u32) -> agentchat_core::model::LaunchIdentity {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(identity) = LaunchOwners::capture(owner, supervisor)
            && LaunchOwners::observe(&identity) == LaunchOwnerState::Current
        {
            return identity;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn positive_native_owner_loss_ignores_surviving_supervisor() {
    let mut owner = Owned::sleep();
    let supervisor = Owned::sleep();
    let identity = capture(owner.pid(), supervisor.pid());
    owner.kill();
    assert_eq!(LaunchOwners::observe(&identity), LaunchOwnerState::Gone);
}
#[test]
fn positive_supervisor_loss_ignores_surviving_native_owner() {
    let owner = Owned::sleep();
    let mut supervisor = Owned::sleep();
    let identity = capture(owner.pid(), supervisor.pid());
    supervisor.kill();
    assert_eq!(LaunchOwners::observe(&identity), LaunchOwnerState::Gone);
}
#[test]
fn same_birth_changed_provenance_and_malformed_proofs_are_unknown() {
    let owner = Owned::sleep();
    let supervisor = Owned::sleep();
    let mut identity = capture(owner.pid(), supervisor.pid());
    let mut value: serde_json::Value = serde_json::from_str(&identity.owner).unwrap();
    value["owner"]["uid"] = serde_json::json!(u32::MAX);
    identity.owner = value.to_string();
    assert_eq!(LaunchOwners::observe(&identity), LaunchOwnerState::Unknown);
    identity.owner = "{}".into();
    assert_eq!(LaunchOwners::observe(&identity), LaunchOwnerState::Unknown);
    assert!(LaunchOwners::capture(owner.pid(), owner.pid()).is_err());
}
