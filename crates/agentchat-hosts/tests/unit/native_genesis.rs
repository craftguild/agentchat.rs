//! Real killed creators at the four durable genesis publication boundaries.
use super::*;
use crate::claude::process::ProcessIdentity;
use std::{
    os::unix::fs::{FileExt, MetadataExt},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

fn namespace(home: &std::path::Path, owner: ProcessIdentity) -> OwnerNamespace {
    let ns = OwnerNamespace::location(home, owner).unwrap();
    let mut path = home.to_owned();
    super::super::private(&path).unwrap();
    for part in ["hosts", "claude", "owners"] {
        path.push(part);
        super::super::private(&path).unwrap();
    }
    super::super::private(ns.directory()).unwrap();
    ns
}
#[test]
#[ignore = "owned subprocess crash fixture; invoked explicitly by its parent"]
fn interrupted_creator_child() {
    let home =
        PathBuf::from(std::env::var_os("AGENTCHAT_GENESIS_TEST_HOME").expect("fixture-only home"));
    let owner: ProcessIdentity =
        serde_json::from_str(&std::env::var("AGENTCHAT_GENESIS_TEST_OWNER").unwrap()).unwrap();
    let phase = std::env::var("AGENTCHAT_GENESIS_TEST_PHASE").unwrap();
    let ns = namespace(&home, owner);
    let file = open(&ns, true, true).unwrap();
    fs2::FileExt::lock_exclusive(&file).unwrap();
    let id = storage::identity(&file.metadata().unwrap());
    if phase != "lock" {
        storage::persist(
            &crate::claude::metadata::directory::Directory::owned(ns.directory()).unwrap(),
            &ns,
            &super::super::state::Header::new(&ns, id),
            None,
        )
        .unwrap();
    }
    if phase == "marker" || phase == "seal" {
        let (record, sealed) = record(&ns, &file).unwrap().unwrap();
        assert!(!sealed);
        mark(&ns, &record).unwrap();
    }
    if phase == "seal" {
        seal(&file).unwrap();
    }
    std::fs::write(
        std::env::var_os("AGENTCHAT_GENESIS_TEST_READY").unwrap(),
        phase.as_bytes(),
    )
    .unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}
#[tokio::test]
async fn killed_creators_recover_pristine_genesis_without_resetting_established_history() {
    for phase in ["lock", "header", "marker", "seal"] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("state");
        let ready = root.path().join("ready");
        let config = root.path().join("config");
        std::fs::create_dir(&config).unwrap();
        let owner = ProcessIdentity::capture(std::process::id()).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "claude::namespace::genesis::tests::interrupted_creator_child",
                "--ignored",
                "--nocapture",
            ])
            .env("AGENTCHAT_GENESIS_TEST_HOME", &home)
            .env(
                "AGENTCHAT_GENESIS_TEST_OWNER",
                serde_json::to_string(&owner).unwrap(),
            )
            .env("AGENTCHAT_GENESIS_TEST_PHASE", phase)
            .env("AGENTCHAT_GENESIS_TEST_READY", &ready)
            .env("AGENTCHAT_HOME", &home)
            .env("AGENTCHAT_USER_CONFIG_ROOT", &config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "creator exited before {phase}"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let ns = OwnerNamespace::location(&home, owner.clone()).unwrap();
        let lock = ns.directory().join(LOCK);
        let lock_id = storage::identity(&std::fs::metadata(&lock).unwrap());
        // Kill the exact owned child while its real kernel file lock is held.
        child.kill().await.unwrap();
        let result = child.wait_with_output().await.unwrap();
        assert!(!result.status.success());
        if phase != "seal" {
            let marker = ns.directory().join(MARKER);
            assert_eq!(marker.exists(), phase == "marker");
            assert!(
                OwnerNamespace::existing(&home, owner.clone())
                    .await
                    .is_err()
            );
            assert_eq!(marker.exists(), phase == "marker");
        }
        let recovered = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
        assert_eq!(
            storage::identity(&std::fs::metadata(&lock).unwrap()),
            lock_id
        );
        assert!(recovered.read_current().await.unwrap().is_none());
        assert_eq!(recovered.generation_floor().await.unwrap(), 0);
        for name in [LOCK, "owner-state.json", MARKER] {
            let metadata = std::fs::metadata(ns.directory().join(name)).unwrap();
            assert_eq!(metadata.nlink(), 1, "{phase}: {name}");
            assert_eq!(metadata.mode() & 0o777, 0o600, "{phase}: {name}");
        }
        let birth = std::fs::read(ns.directory().join(MARKER)).unwrap();
        recovered
            .reserve(uuid::Uuid::new_v4(), root.path().into(), None)
            .await
            .unwrap();
        assert_eq!(recovered.generation_floor().await.unwrap(), 1);
        let state = ns.directory().join("owner-state.json");
        let established = std::fs::read(&state).unwrap();
        std::fs::remove_file(&state).unwrap();
        assert!(OwnerNamespace::open(&home, owner.clone()).await.is_err());
        assert!(!state.exists());
        assert_eq!(std::fs::read(ns.directory().join(MARKER)).unwrap(), birth);
        storage::persist(
            &crate::claude::metadata::directory::Directory::owned(ns.directory()).unwrap(),
            &ns,
            &serde_json::from_slice::<super::super::state::Header>(&established).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(recovered.generation_floor().await.unwrap(), 1);
    }
}
#[tokio::test]
async fn legacy_empty_locks_keep_existing_floors_but_missing_legacy_headers_stay_ambiguous() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let ns = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
    std::fs::remove_file(ns.directory().join(MARKER)).unwrap();
    std::fs::write(ns.directory().join(LOCK), b"").unwrap();
    ns.reserve(uuid::Uuid::new_v4(), root.path().into(), None)
        .await
        .unwrap();
    assert_eq!(ns.generation_floor().await.unwrap(), 1);
    assert!(!ns.directory().join(MARKER).exists());
    std::fs::remove_file(ns.directory().join("owner-state.json")).unwrap();
    assert_eq!(
        OwnerNamespace::open(&home, owner).await.unwrap_err().code,
        agentchat_core::ErrorCode::Busy
    );
    assert!(!ns.directory().join("owner-state.json").exists());
    assert_eq!(
        std::fs::metadata(ns.directory().join(LOCK)).unwrap().len(),
        0
    );
}
#[tokio::test]
async fn a_missing_birth_marker_cannot_erase_nonpristine_floors_or_downgrade_a_new_lock() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let ns = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
    ns.reserve(uuid::Uuid::new_v4(), root.path().into(), None)
        .await
        .unwrap();
    let path = ns.directory().join("owner-state.json");
    let before = std::fs::read(&path).unwrap();
    let marker = ns.directory().join(MARKER);
    // Restore the logical witness through the owned writer below; provenance
    // from the removed inode must not be copied into a new publication.
    let witness: serde_json::Value = crate::claude::metadata::read(&marker).unwrap();
    std::fs::remove_file(&marker).unwrap();
    assert!(OwnerNamespace::open(&home, owner.clone()).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!marker.exists());
    crate::claude::metadata::create(&marker, &witness).unwrap();
    std::fs::write(ns.directory().join(LOCK), b"").unwrap();
    assert!(OwnerNamespace::open(&home, owner).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
#[tokio::test]
async fn a_foreign_birth_marker_and_orphan_staging_data_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let ns = namespace(&home, owner.clone());
    let staging = ns.directory().join(".owner-lock-retained-orphan");
    std::fs::write(&staging, b"interrupted stage").unwrap();
    let opened = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
    assert_eq!(std::fs::read(&staging).unwrap(), b"interrupted stage");
    let marker = ns.directory().join(MARKER);
    let mut foreign: serde_json::Value = crate::claude::metadata::read(&marker).unwrap();
    foreign["namespace_id"] = serde_json::json!(uuid::Uuid::new_v4());
    crate::claude::metadata::write(&marker, &foreign).unwrap();
    let bytes = std::fs::read(&marker).unwrap();
    assert!(OwnerNamespace::open(&home, owner).await.is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), bytes);
    drop(opened);
}

#[tokio::test]
async fn simultaneous_first_creators_share_one_complete_lock_and_birth_witness() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut tasks = vec![];
    for _ in 0..16 {
        let h = home.clone();
        let o = owner.clone();
        tasks.push(tokio::spawn(
            async move { OwnerNamespace::open(&h, o).await },
        ));
    }
    let mut ids = vec![];
    for task in tasks {
        let ns = task.await.unwrap().unwrap();
        ids.push(storage::identity(
            &std::fs::metadata(ns.directory().join(LOCK)).unwrap(),
        ));
        assert_eq!(ns.generation_floor().await.unwrap(), 0);
        assert!(ns.directory().join(MARKER).is_file());
    }
    assert!(ids.iter().all(|id| *id == ids[0]));
}

#[tokio::test]
async fn durable_birth_witness_precedes_even_a_failed_first_callback() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let ns = namespace(&home, owner.clone());
    let result: agentchat_core::Result<()> = storage::access(&ns, |header| {
        assert!(header.pristine());
        let file = storage::open(&ns.directory().join(LOCK), LIMIT, false)?;
        let (record, sealed) = record(&ns, &file)?.unwrap();
        assert!(sealed);
        assert!(marked(&ns, &record)?);
        let committed: super::super::state::Header =
            storage::read_record(&ns.directory().join("owner-state.json"), 262_144)?;
        assert!(committed.pristine());
        Err(unavailable())
    });
    assert!(result.is_err());
    let existing = OwnerNamespace::existing(&home, owner).await.unwrap();
    assert_eq!(existing.generation_floor().await.unwrap(), 0);
    assert!(existing.read_current().await.unwrap().is_none());
}

#[tokio::test]
async fn missing_both_established_records_cannot_reopen_pristine_genesis() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let ns = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
    ns.reserve(uuid::Uuid::new_v4(), root.path().into(), None)
        .await
        .unwrap();
    assert_eq!(ns.generation_floor().await.unwrap(), 1);
    let lock = std::fs::read(ns.directory().join(LOCK)).unwrap();
    std::fs::remove_file(ns.directory().join("owner-state.json")).unwrap();
    std::fs::remove_file(ns.directory().join(MARKER)).unwrap();
    assert!(OwnerNamespace::open(&home, owner.clone()).await.is_err());
    assert!(OwnerNamespace::existing(&home, owner).await.is_err());
    assert!(!ns.directory().join("owner-state.json").exists());
    assert!(!ns.directory().join(MARKER).exists());
    assert_eq!(std::fs::read(ns.directory().join(LOCK)).unwrap(), lock);
}

#[tokio::test]
async fn corrupted_or_rolled_back_seal_cannot_reset_established_floors() {
    for seal_byte in [b'0', b'x'] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("state");
        let owner = ProcessIdentity::capture(std::process::id()).unwrap();
        let ns = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
        ns.reserve(uuid::Uuid::new_v4(), root.path().into(), None)
            .await
            .unwrap();
        let state = std::fs::read(ns.directory().join("owner-state.json")).unwrap();
        let file = storage::open(&ns.directory().join(LOCK), LIMIT, true).unwrap();
        file.write_all_at(&[seal_byte], 0).unwrap();
        file.sync_all().unwrap();
        assert!(OwnerNamespace::open(&home, owner.clone()).await.is_err());
        assert!(OwnerNamespace::existing(&home, owner).await.is_err());
        assert_eq!(
            std::fs::read(ns.directory().join("owner-state.json")).unwrap(),
            state
        );
    }
}
