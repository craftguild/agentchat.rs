//! Real owned helper deaths and filesystem recovery; no native host admission.
use agentchat_hosts::claude;
use agentchat_hosts::claude::{
    metadata,
    namespace::{OwnerNamespace, OwnerReaper, TransportLease},
    process::{BirthState, ProcessIdentity},
};
use std::{
    fs,
    os::unix::{fs::DirBuilderExt, process::ExitStatusExt},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;
#[path = "gateway_recovery/harness.rs"]
mod harness;
use harness::{Gateway, OwnedChild};
#[path = "gateway_recovery/cases.rs"]
mod cases;
#[tokio::test]
async fn a_killed_physical_helper_is_collected_before_rebind_with_a_live_native_owner() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("metadata");
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    metadata::private_directory(&root).unwrap();
    let scope = Uuid::new_v4();
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut gateway = Gateway::spawn(&root, scope, owner.pid(), &base.path().join("ready"));
    assert_ne!(gateway.helper, owner);
    gateway.kill();
    assert!(owner.is_current());
    let next = TransportLease::bind(&root, scope, &owner).await.unwrap();
    assert_ne!(next.nonce, gateway.endpoint.nonce);
    assert!(
        !gateway.endpoint.socket.parent().unwrap().exists(),
        "Rebind discarded its endpoint witness but orphaned the original socket directory"
    );
    assert!(owner.is_current());
    drop(next);
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[tokio::test]
async fn dead_owner_collection_reclaims_the_original_gateway_before_discarding_its_witness() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().canonicalize().unwrap().join("state");
    let mut native = OwnedChild(
        Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let expected = Path::new("/bin/cat").canonicalize().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let owner = loop {
        if let Ok(value) = ProcessIdentity::capture(native.0.id())
            && value.executable().canonicalize().unwrap() == expected
            && value.is_current()
        {
            break value;
        }
        assert!(native.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "owned native image readiness");
        std::thread::sleep(Duration::from_millis(5));
    };
    let ns = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
    let mut gateway = Gateway::spawn(
        ns.directory(),
        ns.id(),
        owner.pid(),
        &base.path().join("ready"),
    );
    gateway.kill();
    native.0.kill().unwrap();
    assert_eq!(native.0.wait().unwrap().signal(), Some(9));
    assert_eq!(owner.birth_state(), BirthState::Gone);
    let mut reaper = OwnerReaper::new(&home);
    let mut directories = 0;
    for _ in 0..64 {
        let report = reaper.step().unwrap();
        assert!(report.visited <= 8 && report.removed <= 40);
        assert!(report.directories_removed <= 1);
        directories += report.directories_removed;
    }
    assert!(
        !gateway.endpoint.socket.parent().unwrap().exists(),
        "Dead-owner GC discarded its gateway witness without collecting the original socket directory"
    );
    assert!(!ns.directory().exists());
    assert_eq!(
        directories, 2,
        "external socket directory and original quarantine"
    );
    for entry in fs::read_dir(home.join("hosts/claude/owners")).unwrap() {
        assert_eq!(entry.unwrap().file_name(), ".collection.lock");
    }
}
