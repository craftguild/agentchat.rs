//! Pending UUID allocations preserve original owner floors across helper retry.
use agentchat_core::{
    ErrorCode,
    model::{Context, HostKind},
};
use agentchat_hosts::claude::{
    NativeLifecycle, NativeRegistration, metadata,
    namespace::{OwnerNamespace, Reservation},
    process::ProcessIdentity,
};
use serde_json::Value;
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf};
use uuid::Uuid;
struct Fixture {
    _root: tempfile::TempDir,
    ns: OwnerNamespace,
    cwd: PathBuf,
    project: Uuid,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        fs::create_dir(&cwd).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let ns = OwnerNamespace::open(
            &root.path().join("state"),
            ProcessIdentity::capture(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
        Self {
            _root: root,
            ns,
            cwd,
            project: Uuid::new_v4(),
        }
    }
    async fn reserve(&self) -> Reservation {
        self.ns
            .reserve(Uuid::new_v4(), self.cwd.clone(), None)
            .await
            .unwrap()
    }
    fn proposed(&self, r: &Reservation) -> Context {
        Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            project_id: self.project,
            host: HostKind::Claude,
            host_session_id: r.root.to_string(),
            host_version: "2.1.301".into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: self.cwd.clone(),
        }
    }
    fn project(&self, r: &Reservation, c: Context) -> NativeLifecycle {
        NativeLifecycle::Project(NativeRegistration {
            owner_root: self.ns.id(),
            observer: self.ns.observer().into(),
            generation: r.generation,
            root: r.root,
            context: c,
        })
    }
    fn header(&self) -> Value {
        metadata::read(&self.ns.directory().join("owner-state.json")).unwrap()
    }
    fn write_raw_header(&self, value: &Value) {
        // Emulate the actual versioned owner-header producer, including legacy
        // and corrupt inputs. Generic native records have a different format.
        fs::write(
            self.ns.directory().join("owner-state.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test]
async fn legacy_pending_upgrade_keeps_original_lock_marker_and_all_ordering_floors() {
    let f = Fixture::new().await;
    let a = f.reserve().await;
    let old = f.proposed(&a);
    f.ns.commit(a.clone(), f.project(&a, old)).await.unwrap();
    f.ns.prepare_call(
        a.root,
        a.generation,
        "mcp__agentchat__agentchat_status".into(),
        "kept_call".into(),
    )
    .await
    .unwrap();
    f.ns.observation_sequence(a.root, a.generation)
        .await
        .unwrap();
    let before = f.header();
    let lock = f.ns.directory().join("owner-state.lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    let lock_bytes = fs::read(&lock).unwrap();
    let marker = fs::read(f.ns.directory().join("owner-state.initialized.json")).unwrap();
    let b = f.reserve().await;
    let mut legacy = f.header();
    legacy["version"] = 1.into();
    legacy.as_object_mut().unwrap().remove("allocation");
    f.write_raw_header(&legacy);
    let expected = f.proposed(&b);
    let chosen =
        f.ns.allocate_context(b.clone(), expected.clone())
            .await
            .unwrap();
    assert_eq!(chosen, expected);
    assert_eq!(chosen.context_id.get_version(), Some(uuid::Version::Random));
    assert_eq!(
        chosen.incarnation.get_version(),
        Some(uuid::Version::Random)
    );
    assert_eq!(
        f.ns.allocate_context(b.clone(), f.proposed(&b))
            .await
            .unwrap(),
        chosen
    );
    let after = f.header();
    assert_eq!(after["version"], 2);
    assert_eq!(after["generation"], b.generation);
    assert_eq!(after["call_sequence"], before["call_sequence"]);
    assert_eq!(
        after["observation_sequence"],
        before["observation_sequence"]
    );
    assert_eq!(fs::metadata(&lock).unwrap().ino(), inode);
    assert_eq!(fs::read(lock).unwrap(), lock_bytes);
    assert_eq!(
        fs::read(f.ns.directory().join("owner-state.initialized.json")).unwrap(),
        marker
    );
    f.ns.commit(b.clone(), f.project(&b, chosen)).await.unwrap();
    assert!(f.header().get("allocation").is_none());
}
#[tokio::test]
async fn concurrent_helpers_choose_the_same_random_pair_for_one_original_intent() {
    let f = Fixture::new().await;
    let intent = f.reserve().await;
    let mut jobs = Vec::new();
    for _ in 0..32 {
        let ns = f.ns.clone();
        let r = intent.clone();
        let proposed = f.proposed(&intent);
        jobs.push(tokio::spawn(async move {
            ns.allocate_context(r, proposed).await.unwrap()
        }));
    }
    let first = jobs.remove(0).await.unwrap();
    for job in jobs {
        assert_eq!(job.await.unwrap(), first);
    }
    assert_ne!(first.context_id, first.incarnation);
    assert_ne!(first.context_id, intent.ticket);
    assert_ne!(first.incarnation, intent.ticket);
    let before = f.header();
    f.ns.commit(intent.clone(), f.project(&intent, first.clone()))
        .await
        .unwrap();
    assert_eq!(
        f.ns.allocate_context(intent.clone(), f.proposed(&intent))
            .await
            .unwrap(),
        first
    );
    assert_eq!(f.header()["generation"], before["generation"]);
    assert!(f.header().get("allocation").is_none());
}
#[tokio::test]
async fn changed_source_and_wrong_committed_references_cannot_rebind_the_pending_allocation() {
    let f = Fixture::new().await;
    let intent = f.reserve().await;
    let context =
        f.ns.allocate_context(intent.clone(), f.proposed(&intent))
            .await
            .unwrap();
    let bytes = fs::read(f.ns.directory().join("owner-state.json")).unwrap();
    let mut changed = f.proposed(&intent);
    changed.project_id = Uuid::new_v4();
    assert_eq!(
        f.ns.allocate_context(intent.clone(), changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        f.ns.commit(intent.clone(), f.project(&intent, f.proposed(&intent)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        fs::read(f.ns.directory().join("owner-state.json")).unwrap(),
        bytes
    );
    f.ns.commit(intent.clone(), f.project(&intent, context))
        .await
        .unwrap();
}
#[tokio::test]
async fn missing_incompatible_or_corrupt_format_two_allocation_never_regenerates_references() {
    let f = Fixture::new().await;
    let intent = f.reserve().await;
    f.ns.allocate_context(intent.clone(), f.proposed(&intent))
        .await
        .unwrap();
    let original = f.header();
    let path = f.ns.directory().join("owner-state.json");
    let mut cases = Vec::new();
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("allocation");
    cases.push(missing);
    let mut foreign = original.clone();
    foreign["allocation"]["ticket"] = Uuid::new_v4().to_string().into();
    cases.push(foreign);
    let mut incomplete = original.clone();
    incomplete["allocation"]
        .as_object_mut()
        .unwrap()
        .remove("source_sha256");
    cases.push(incomplete);
    let mut downgrade = original.clone();
    downgrade["version"] = 1.into();
    cases.push(downgrade);
    let mut nil = original.clone();
    nil["allocation"]["context_id"] = Uuid::nil().to_string().into();
    cases.push(nil);
    for value in cases {
        f.write_raw_header(&value);
        let bytes = fs::read(&path).unwrap();
        assert_eq!(
            f.ns.generation_floor().await.unwrap_err().code,
            ErrorCode::SessionRequired
        );
        assert!(
            f.ns.allocate_context(intent.clone(), f.proposed(&intent))
                .await
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    f.write_raw_header(&original);
    assert_eq!(f.ns.generation_floor().await.unwrap(), intent.generation);
}
#[tokio::test]
async fn superseding_or_cancelling_an_intent_discards_only_its_pending_allocation() {
    let f = Fixture::new().await;
    let first = f.reserve().await;
    let a =
        f.ns.allocate_context(first.clone(), f.proposed(&first))
            .await
            .unwrap();
    let second = f.reserve().await;
    assert_eq!(f.header()["allocation"]["state"], "unassigned");
    assert!(
        f.ns.allocate_context(first.clone(), f.proposed(&first))
            .await
            .is_err()
    );
    let b =
        f.ns.allocate_context(second.clone(), f.proposed(&second))
            .await
            .unwrap();
    assert_ne!(a.context_id, b.context_id);
    assert_ne!(a.incarnation, b.incarnation);
    f.ns.retire(second.root, second.generation).await.unwrap();
    let cancelled = f.header();
    assert!(cancelled.get("allocation").is_none());
    assert!(cancelled["pending"].is_null());
    assert_eq!(cancelled["generation"], second.generation);
    assert!(
        f.ns.allocate_context(second.clone(), f.proposed(&second))
            .await
            .is_err()
    );
}
