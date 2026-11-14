//! Real killed cleanup workers plus injected unavailable birth observations.
use super::*;
use crate::claude::observer::relay;
#[allow(dead_code)] // The same owned process fixtures also serve integration tests.
#[path = "../relay_recovery/support.rs"]
mod support;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::{fs::OpenOptionsExt, process::ExitStatusExt},
    path::PathBuf,
    process::Child,
};

const RELAY: &str = "claude::observer::relay::recovery::tests::support::relay_process_child";
const NATIVE: &str = "claude::observer::relay::recovery::tests::support::native_owner_child";

#[path = "native_relay_inherited.rs"]
mod inherited;

pub(super) fn cut(phase: &str, root: &Path) {
    if std::env::var("AGENTCHAT_RELAY_RECOVERY_CUT").as_deref() != Ok(phase)
        || std::env::var_os("AGENTCHAT_RELAY_TEST_ROOT").as_deref() != Some(root.as_os_str())
    {
        return;
    }
    let report = PathBuf::from(std::env::var_os("AGENTCHAT_RELAY_CUT_REPORT").unwrap());
    let temporary = report.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap();
    file.write_all(phase.as_bytes()).unwrap();
    file.sync_all().unwrap();
    fs::rename(temporary, report).unwrap();
    loop {
        std::thread::park();
    }
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn preserved(fixture: &support::Fixture, before: &[(PathBuf, (u64, u64))], bytes: &[u8]) {
    assert_eq!(fs::read(fixture.endpoint_path()).unwrap(), bytes);
    for (path, id) in before {
        assert_eq!(support::identity(path), *id);
    }
}

#[tokio::test]
async fn unknown_relay_birth_preserves_a_complete_original_publication() {
    let mut fixture = support::Fixture::new(RELAY).await;
    fixture.crash();
    let before = fixture.snapshot();
    let bytes = fs::read(fixture.endpoint_path()).unwrap();
    assert!(
        retire_with(&fixture.endpoint_path(), "observer", &fixture.owner, |_| {
            BirthState::Unknown
        })
        .await
        .is_err()
    );
    preserved(&fixture, &before, &bytes);
    retire(&fixture.endpoint_path(), "observer", &fixture.owner)
        .await
        .unwrap();
    assert!(!fixture.directory.exists());
}

#[tokio::test]
async fn unknown_unequal_native_birth_preserves_original_endpoint_and_sockets() {
    let mut native = support::NativeOwner::new(NATIVE).await;
    let mut fixture = support::Fixture::with_owner(RELAY, native.token.clone()).await;
    fixture.crash();
    native.exit();
    let before = fixture.snapshot();
    let bytes = fs::read(fixture.endpoint_path()).unwrap();
    let current = ProcessIdentity::capture(std::process::id()).unwrap();
    assert!(
        retire_with(&fixture.endpoint_path(), "observer", &current, |_| {
            BirthState::Unknown
        })
        .await
        .is_err()
    );
    preserved(&fixture, &before, &bytes);
    retire(&fixture.endpoint_path(), "observer", &current)
        .await
        .unwrap();
}

#[tokio::test]
async fn four_actual_sigkill_cleanup_prefixes_recover_without_resetting_identity() {
    for phase in ["request.sock", "reply.sock", "directory", "endpoint"] {
        let mut fixture = support::Fixture::new(RELAY).await;
        fixture.crash();
        let report = fixture.root.path().join("cleanup-cut");
        let mut command = support::Fixture::command(RELAY, fixture.root.path(), &fixture.owner);
        command
            .env("AGENTCHAT_RELAY_TEST_ACTION", "retire")
            .env("AGENTCHAT_RELAY_RECOVERY_CUT", phase)
            .env("AGENTCHAT_RELAY_CUT_REPORT", &report);
        let mut child = Owned(command.spawn().unwrap());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !report.exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "Cleanup exited before {phase}"
            );
            assert!(
                tokio::time::Instant::now() < deadline,
                "Cleanup missed {phase}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(fs::read(&report).unwrap(), phase.as_bytes());
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        retire(&fixture.endpoint_path(), "observer", &fixture.owner)
            .await
            .unwrap();
        assert!(!fixture.endpoint_path().exists());
        assert!(!fixture.directory.exists());
        assert!(fixture.owner.is_current());
    }
}
