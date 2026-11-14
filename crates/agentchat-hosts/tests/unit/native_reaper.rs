//! Actual owned process lifetimes; metadata fixtures do not admit a native host.
use super::*;
use crate::claude::{
    NativeRegistration,
    namespace::{LifecycleOwner, genesis, private, state::Header, storage},
    observer::{NativeProfile, RegisteredOwner},
    process::{BirthState, ProcessIdentity},
    routing,
};
use agentchat_core::model::{Context, HostKind};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::MetadataExt, process::ExitStatusExt},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

struct Fixture {
    root: TempDir,
    home: PathBuf,
    child: OwnedChild,
    ns: OwnerNamespace,
    registration: NativeRegistration,
}
struct OwnedChild(Child);
impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
impl Fixture {
    async fn new(exec: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("state");
        let project = base.join("project");
        fs::create_dir(&project).unwrap();
        let ready = base.join("exec-ready");
        let mut command = if exec {
            let mut c = Command::new("/bin/sh");
            c.args([
                "-c",
                "printf ready > \"$AGENTCHAT_REAPER_EXEC_READY\"; read -r line; exec /bin/sleep 60",
            ]);
            c.env("AGENTCHAT_REAPER_EXEC_READY", &ready);
            c
        } else {
            Command::new("/bin/cat")
        };
        let mut child = OwnedChild(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        if exec {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.exists() {
                assert!(child.try_wait().unwrap().is_none());
                assert!(Instant::now() < deadline, "shell startup readiness");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // Shell main has published its owned readiness marker above. For direct
        // cat, observe its exact image first. Preserve the actual full current
        // token in both cases rather than imposing a shell-image alias.
        let expected = (!exec).then(|| std::path::Path::new("/bin/cat").canonicalize().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        let owner = loop {
            if let Ok(owner) = ProcessIdentity::capture(child.id())
                && expected.as_ref().is_none_or(|path| {
                    owner.executable().canonicalize().ok().as_ref() == Some(path)
                })
                && owner.is_current()
            {
                break owner;
            }
            assert!(child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "fixture image readiness");
            std::thread::sleep(Duration::from_millis(5));
        };
        let ns = OwnerNamespace::open(&home, owner.clone()).await.unwrap();
        let root_id = Uuid::new_v4();
        let intent = ns.reserve(root_id, project.clone(), None).await.unwrap();
        let registration = NativeRegistration {
            owner_root: ns.id,
            observer: ns.observer.clone(),
            generation: intent.generation,
            root: root_id,
            context: Context {
                context_id: Uuid::new_v4(),
                project_id: Uuid::new_v4(),
                host: HostKind::Claude,
                host_session_id: root_id.to_string(),
                host_version: "2.1.301".into(),
                os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                working_directory: project,
                incarnation: Uuid::new_v4(),
            },
        };
        ns.commit(intent, NativeLifecycle::Project(registration.clone()))
            .await
            .unwrap();
        ns.prepare_call(
            root_id,
            registration.generation,
            "mcp__agentchat__agentchat_status".into(),
            "retained_call".into(),
        )
        .await
        .unwrap();
        ns.observation_sequence(root_id, registration.generation)
            .await
            .unwrap();
        ns.publish_profile(NativeProfile {
            executable: owner.executable(),
            version: "2.1.301".into(),
        })
        .await
        .unwrap();
        drop(files::locked(&ns.directory.join(format!("gateway-{}.lock", ns.id)), true).unwrap());
        let registered = RegisteredOwner {
            registration: registration.clone(),
            owner: owner.clone(),
        };
        metadata::write(
            &ns.directory
                .join(format!("context-{}.json", registration.context.context_id)),
            &registered,
        )
        .unwrap();
        metadata::write(
            &ns.directory.join(format!("native-{}.json", ns.observer)),
            &registered,
        )
        .unwrap();
        metadata::write(
            &ns.directory.join(format!("current-owner-{}.json", ns.id)),
            &LifecycleOwner {
                registration: NativeLifecycle::Project(registration.clone()),
                owner,
            },
        )
        .unwrap();
        routing::publish(&home, &registered).unwrap();
        let context = &registration.context;
        let token = Uuid::new_v4();
        metadata::write(&ns.directory.join(format!("wake-{}.json", context.context_id)), &json!({
            "root":root_id,"context_id":context.context_id,"incarnation":context.incarnation,
            "generation":777,"token":token,"expected_turn_id":null,
            "hint":{"version":1,"project_id":context.project_id,"context_id":context.context_id,"incarnation":context.incarnation,
                "agent_id":Uuid::new_v4(),"batch_id":Uuid::new_v4(),"events":[],"pending_count":0,"preview":"fixture"}
        })).unwrap();
        metadata::write(&ns.directory.join(format!("submitted-{}.json", context.context_id)), &json!({
            "accepted":true,"token":token,"context_id":context.context_id,"incarnation":context.incarnation,
            "root":root_id,"generation":777,"unix_ms":1
        })).unwrap();
        private(&home.join("contexts")).unwrap();
        metadata::write(
            &home
                .join("contexts")
                .join(format!("{}.json", context.context_id)),
            &json!({"retained":"initialization retry identity"}),
        )
        .unwrap();
        Self {
            root,
            home,
            child,
            ns,
            registration,
        }
    }
    fn stop(&mut self, abrupt: bool) {
        if abrupt {
            self.child.kill().unwrap();
        } else {
            drop(self.child.stdin.take());
        }
        let status = self.child.wait().unwrap();
        if abrupt {
            assert_eq!(status.signal(), Some(9));
        } else {
            assert!(status.success());
        }
        assert_eq!(self.ns.owner.birth_state(), BirthState::Gone);
    }
    fn quarantine(&self) -> PathBuf {
        self.home
            .join("hosts/claude/owners")
            .join(format!(".retired-{}", proof::key(&self.ns).unwrap()))
    }
    fn certificate(&self) -> PathBuf {
        proof::certificate_path(&self.home.join("hosts/claude/owners"), &self.ns).unwrap()
    }
    fn gone(&self) -> bool {
        !self.ns.directory.exists() && !self.quarantine().exists() && !self.certificate().exists()
    }
    fn history(&self) -> PathBuf {
        self.home
            .join("contexts")
            .join(format!("{}.json", self.registration.context.context_id))
    }
}
fn bounded(report: &ReapReport) {
    assert!(
        report.visited <= 8 && report.removed <= 40 && report.directories_removed <= 1,
        "{report:?}"
    );
    // Maintenance intentionally does not wait on a lock. Give overlapping
    // process/lock fixtures scheduling time between attempts instead of treating
    // a fixed number of tight spins as elapsed readiness time.
    std::thread::sleep(Duration::from_millis(2));
}
fn drain(f: &Fixture) {
    let mut reaper = OwnerReaper::new(&f.home);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(20);
    let mut reports = Vec::new();
    while Instant::now() < deadline {
        let report = reaper.step().unwrap();
        bounded(&report);
        if reports.len() < 256 {
            reports.push(report);
        }
        if f.gone() {
            eprintln!(
                "collector ready in {:?}; reports={reports:?}",
                started.elapsed()
            );
            return;
        }
    }
    panic!(
        "collector did not finish after {:?}, birth={:?}, reports={reports:?}, entries={:?}",
        started.elapsed(),
        f.ns.owner.birth_state(),
        fs::read_dir(f.home.join("hosts/claude/owners"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>()
    );
}
#[tokio::test]
async fn dead_owner_projections_are_collected_without_touching_history_or_signal() {
    let mut f = Fixture::new(false).await;
    let history = fs::read(f.history()).unwrap();
    let signal = f
        .registration
        .context
        .working_directory
        .join(".agentchat-wakeup");
    let bytes = fs::read(&signal).unwrap();
    let inode = fs::metadata(&signal).unwrap().ino();
    let sentinel = f
        .registration
        .context
        .working_directory
        .join(".agentchat-wakeup-stage-foreign");
    fs::write(&sentinel, b"foreign").unwrap();
    f.stop(false);
    drain(&f);
    assert_eq!(fs::read(f.history()).unwrap(), history);
    assert_eq!(fs::read(&signal).unwrap(), bytes);
    assert_eq!(fs::metadata(signal).unwrap().ino(), inode);
    assert_eq!(fs::read(sentinel).unwrap(), b"foreign");
    assert!(
        !f.home
            .join("hosts/claude/contexts")
            .join(format!("{}.json", f.registration.context.context_id))
            .exists()
    );
}

#[tokio::test]
async fn current_original_capsules_require_positive_owner_loss_before_bounded_collection() {
    let mut f = Fixture::new(false).await;
    let publisher = f.ns.metadata_publisher().unwrap();
    let file = crate::claude::body::tests::input();
    file.publish(&publisher, &f.ns.directory).unwrap();
    crate::claude::body::InputHead::new(&file)
        .unwrap()
        .publish(&publisher, &f.ns.directory)
        .unwrap();
    crate::claude::body::InputReceipt::from_native(
        &file,
        crate::claude::body::NativeInputResult::Append(json!({
            "uuid":Uuid::new_v4(), "message":{"type":"user", "role":"user", "isMeta":true,
            "content":[{"type":"text", "text":file.text}]}
        })),
    )
    .unwrap()
    .publish(&publisher, &f.ns.directory)
    .unwrap();
    let bytes = fs::read(file.path(&f.ns.directory)).unwrap();
    let mut reaper = OwnerReaper::new(&f.home);
    for _ in 0..8 {
        bounded(&reaper.step().unwrap());
    }
    assert_eq!(fs::read(file.path(&f.ns.directory)).unwrap(), bytes);
    assert_eq!(
        crate::claude::body::InputFile::read(&file.path(&f.ns.directory)).unwrap(),
        file
    );
    let history = fs::read(f.history()).unwrap();
    f.stop(false);
    drain(&f);
    assert_eq!(fs::read(f.history()).unwrap(), history);
}
#[tokio::test]
async fn live_and_same_birth_exec_preserve_original_floors() {
    let mut f = Fixture::new(true).await;
    let original = fs::read(f.ns.directory.join("owner-state.json")).unwrap();
    let mut reaper = OwnerReaper::new(&f.home);
    for _ in 0..16 {
        bounded(&reaper.step().unwrap());
    }
    f.child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"exec\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(token) = ProcessIdentity::capture(f.child.id())
            && token.executable() != f.ns.owner.executable()
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!f.ns.owner.is_current());
    assert_eq!(f.ns.owner.birth_state(), BirthState::Present);
    for _ in 0..16 {
        bounded(&reaper.step().unwrap());
    }
    assert_eq!(
        fs::read(f.ns.directory.join("owner-state.json")).unwrap(),
        original
    );
    assert!(!f.quarantine().exists() && !f.certificate().exists());
    f.stop(true);
    drain(&f);
}
#[tokio::test]
async fn killed_owner_is_not_renewed_by_a_live_observer_or_busy_gateway_lock() {
    let mut f = Fixture::new(false).await;
    let path = f.ns.directory.join(format!("gateway-{}.lock", f.ns.id));
    let gateway = files::locked(&path, true).unwrap().unwrap();
    f.stop(true);
    assert!(
        ProcessIdentity::capture(std::process::id())
            .unwrap()
            .is_current()
    );
    assert!(
        OwnerNamespace::open(&f.home, f.ns.owner.clone())
            .await
            .is_err()
    );
    let mut reaper = OwnerReaper::new(&f.home);
    for _ in 0..16 {
        bounded(&reaper.step().unwrap());
    }
    assert!(f.ns.directory.exists() && !f.quarantine().exists());
    drop(gateway);
    drain(&f);
}
#[tokio::test]
async fn foreign_values_aliases_and_quarantine_collisions_survive() {
    let mut f = Fixture::new(false).await;
    let current =
        f.ns.directory
            .join(format!("current-owner-{}.json", f.ns.id));
    let mut value: Value = metadata::read(&current).unwrap();
    value["registration"]["registration"]["root"] = json!(Uuid::new_v4());
    metadata::write(&current, &value).unwrap();
    let changed = fs::read(&current).unwrap();
    let outside = f.root.path().join("foreign-sentinel");
    fs::write(&outside, b"do not delete").unwrap();
    std::os::unix::fs::symlink(&outside, f.ns.directory.join("foreign-link")).unwrap();
    fs::hard_link(
        f.ns.directory.join("native-profile.json"),
        f.root.path().join("profile-alias"),
    )
    .unwrap();
    f.stop(false);
    let mut reaper = OwnerReaper::new(&f.home);
    for _ in 0..64 {
        bounded(&reaper.step().unwrap());
    }
    assert_eq!(
        fs::read(f.quarantine().join(current.file_name().unwrap())).unwrap(),
        changed
    );
    assert!(
        fs::symlink_metadata(f.quarantine().join("foreign-link"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(fs::read(outside).unwrap(), b"do not delete");
    assert!(f.quarantine().join("native-profile.json").exists());
    assert!(f.quarantine().join("owner-state.lock").exists() && f.certificate().exists());

    let mut collision = Fixture::new(false).await;
    fs::create_dir(collision.quarantine()).unwrap();
    fs::write(collision.quarantine().join("user"), b"foreign directory").unwrap();
    let original = fs::read(collision.ns.directory.join("owner-state.json")).unwrap();
    collision.stop(false);
    let mut reaper = OwnerReaper::new(&collision.home);
    for _ in 0..32 {
        bounded(&reaper.step().unwrap());
    }
    assert_eq!(
        fs::read(collision.ns.directory.join("owner-state.json")).unwrap(),
        original
    );
    assert_eq!(
        fs::read(collision.quarantine().join("user")).unwrap(),
        b"foreign directory"
    );
}
#[tokio::test]
async fn cleaning_never_uses_a_successor_directory_as_its_proof() {
    let mut f = Fixture::new(false).await;
    f.stop(false);
    let moved = f.root.path().join("original-quarantine");
    let mut reaper = OwnerReaper::new(&f.home);
    let mut replaced = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !replaced && Instant::now() < deadline {
        let report = reaper
            .step_observed(&mut |point| {
                if point != "validated" || replaced {
                    return;
                }
                fs::rename(f.quarantine(), &moved).unwrap();
                private(&f.quarantine()).unwrap();
                // Copy exact matching tuples into a different directory inode:
                // matching JSON alone must not authorize deleting the successor.
                for entry in fs::read_dir(&moved).unwrap() {
                    let entry = entry.unwrap();
                    let target = f.quarantine().join(entry.file_name());
                    fs::copy(entry.path(), &target).unwrap();
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(target, fs::Permissions::from_mode(0o600)).unwrap();
                }
                replaced = true;
            })
            .unwrap();
        bounded(&report);
    }
    assert!(replaced);
    let expected = fs::read_dir(f.quarantine())
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect::<Vec<_>>();
    let mut fresh = OwnerReaper::new(&f.home);
    for _ in 0..32 {
        bounded(&fresh.step().unwrap());
    }
    for (path, bytes) in expected {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    let index = f
        .home
        .join("hosts/claude/contexts")
        .join(format!("{}.json", f.registration.context.context_id));
    assert!(index.exists() && f.certificate().exists());
}
#[tokio::test]
async fn concurrent_collectors_preserve_a_changed_lookup_and_live_writer_floors() {
    let mut f = Fixture::new(false).await;
    let live_owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let live_ns = OwnerNamespace::open(&f.home, live_owner.clone())
        .await
        .unwrap();
    let root = f.home.join("hosts/claude/owners");
    let index = f
        .home
        .join("hosts/claude/contexts")
        .join(format!("{}.json", f.registration.context.context_id));
    let mut successor: Value = metadata::read(&index).unwrap();
    successor["native"]["owner"] = serde_json::to_value(live_owner).unwrap();
    successor["native"]["registration"]["generation"] = json!(1000);
    metadata::write(&index, &successor).unwrap();
    let expected = fs::read(&index).unwrap();
    f.stop(false);
    let held = files::locked(&root.join(".collection.lock"), true).unwrap();
    assert!(OwnerReaper::new(&f.home).step().unwrap().busy);
    assert!(f.ns.directory.exists());
    drop(held);
    let home = f.home.clone();
    let peer = std::thread::spawn(move || {
        let mut reaper = OwnerReaper::new(home);
        for _ in 0..256 {
            bounded(&reaper.step().unwrap());
            std::thread::yield_now();
        }
    });
    for _ in 0..32 {
        // A real live writer advances its own sealed header while two independent
        // collectors serialize dead-owner finalization under the shared lock.
        let intent = live_ns
            .reserve(
                Uuid::new_v4(),
                f.registration.context.working_directory.clone(),
                None,
            )
            .await
            .unwrap();
        assert!(intent.generation > 0);
    }
    peer.join().unwrap();
    drain(&f);
    assert_eq!(fs::read(index).unwrap(), expected);
    assert_eq!(live_ns.owner.birth_state(), BirthState::Present);
    let (header, _): (Header, _) =
        files::record(&live_ns.directory.join("owner-state.json")).unwrap();
    assert_eq!(header.generation, 32);
}
#[tokio::test]
async fn finalizing_certificate_preserves_changed_witness_or_replaced_directory() {
    let mut f = Fixture::new(false).await;
    f.stop(false);
    let mut reaper = OwnerReaper::new(&f.home);
    let mut observed = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !observed && Instant::now() < deadline {
        let report = reaper
            .step_observed(&mut |point| {
                if point == "finalizing" {
                    observed = true;
                    fs::write(f.quarantine().join("owner-state.json"), b"foreign header").unwrap();
                }
            })
            .unwrap();
        bounded(&report);
    }
    assert!(observed && f.certificate().exists());
    let mut fresh = OwnerReaper::new(&f.home);
    for _ in 0..16 {
        bounded(&fresh.step().unwrap());
    }
    assert_eq!(
        fs::read(f.quarantine().join("owner-state.json")).unwrap(),
        b"foreign header"
    );
    let moved = f.root.path().join("old-quarantine");
    fs::rename(f.quarantine(), &moved).unwrap();
    fs::create_dir(f.quarantine()).unwrap();
    fs::write(f.quarantine().join("successor"), b"new directory inode").unwrap();
    for _ in 0..16 {
        bounded(&fresh.step().unwrap());
    }
    assert_eq!(
        fs::read(f.quarantine().join("successor")).unwrap(),
        b"new directory inode"
    );
}
fn synthetic(home: &std::path::Path, owner: ProcessIdentity) -> OwnerNamespace {
    let ns = OwnerNamespace::locator(home, owner).unwrap();
    private(&ns.directory).unwrap();
    let lock = genesis::open(&ns, true, true).unwrap();
    files::exclusive(&lock).unwrap();
    let (record, sealed) = genesis::record(&ns, &lock).unwrap().unwrap();
    assert!(!sealed);
    let header = Header::new(&ns, storage::identity(&lock.metadata().unwrap()));
    files::publish(
        &ns.home,
        &ns.directory.join("owner-state.json"),
        &header,
        None,
        None,
    )
    .unwrap();
    genesis::mark(&ns, &record).unwrap();
    genesis::seal(&lock).unwrap();
    ns
}
#[tokio::test]
async fn retained_scan_passes_live_foreign_front_entries_and_preserves_unknown_births() {
    let mut f = Fixture::new(false).await;
    let root = f.home.join("hosts/claude/owners");
    for i in 0..32 {
        fs::create_dir(root.join(format!("!foreign-{i}"))).unwrap();
    }
    let live = serde_json::to_value(ProcessIdentity::capture(std::process::id()).unwrap()).unwrap();
    let mut alive = Vec::new();
    for i in 1..=16 {
        let mut value = live.clone();
        value["uid"] = json!(90000 + i);
        alive.push(synthetic(&f.home, serde_json::from_value(value).unwrap()));
    }
    let mut unknown = live.clone();
    unknown["pid"] = json!(0);
    let unknown = synthetic(&f.home, serde_json::from_value(unknown).unwrap());
    let dead = serde_json::to_value(&f.ns.owner).unwrap();
    f.stop(false);
    let mut gone = vec![f.ns.directory.clone()];
    for i in 1..=16 {
        let mut value = dead.clone();
        value["uid"] = json!(80000 + i);
        gone.push(synthetic(&f.home, serde_json::from_value(value).unwrap()).directory);
    }
    let mut reaper = OwnerReaper::new(&f.home);
    for _ in 0..512 {
        bounded(&reaper.step().unwrap());
        if gone.iter().all(|p| !p.exists())
            && !fs::read_dir(&root)
                .unwrap()
                .any(|p| p.unwrap().file_name().to_string_lossy().starts_with(".gc-"))
        {
            break;
        }
    }
    assert!(gone.iter().all(|p| !p.exists()));
    assert!(
        alive
            .iter()
            .all(|n| n.directory.join("owner-state.lock").exists())
    );
    assert!(unknown.directory.join("owner-state.json").exists());
    assert!(root.join("!foreign-0").exists());
}
#[tokio::test]
async fn killed_collector_resumes_every_owned_deletion_prefix() {
    for point in [
        "quarantined",
        "certified",
        "validated",
        "projection",
        "finalizing",
        "owner-state.initialized.json",
        "owner-state.json",
        "gateway_removed",
        "owner-state.lock",
        "directory_removed",
        "certificate_removed",
    ] {
        let mut f = Fixture::new(false).await;
        f.stop(false);
        let marker = f.root.path().join("cut.json");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "claude::namespace::reap::tests::collector_process_child",
                "--nocapture",
            ])
            .env("AGENTCHAT_REAPER_TEST_HOME", &f.home)
            .env("AGENTCHAT_REAPER_TEST_POINT", point)
            .env("AGENTCHAT_REAPER_TEST_MARKER", &marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !marker.exists() {
            if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("collector missed {point}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(9));
        drain(&f);
        assert!(f.history().exists());
    }
}
#[test]
fn missing_roots_are_inert_and_relative_paths_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("missing");
    assert_eq!(
        OwnerReaper::new(&home).step().unwrap(),
        ReapReport::default()
    );
    assert!(!home.exists());
    assert!(OwnerReaper::new("relative-state").step().is_err());
}
#[test]
#[ignore = "owned collector process; parent SIGKILLs each durable cut"]
fn collector_process_child() {
    let home = PathBuf::from(std::env::var_os("AGENTCHAT_REAPER_TEST_HOME").unwrap());
    let marker = PathBuf::from(std::env::var_os("AGENTCHAT_REAPER_TEST_MARKER").unwrap());
    let point = std::env::var("AGENTCHAT_REAPER_TEST_POINT").unwrap();
    let mut reaper = OwnerReaper::new(home);
    for _ in 0..128 {
        reaper
            .step_observed(&mut |actual| {
                if actual == point {
                    fs::write(&marker, actual).unwrap();
                    std::io::stdin().read_exact(&mut [0]).unwrap();
                    panic!("parent must kill collector");
                }
            })
            .unwrap();
    }
    panic!("durable collector cut was not reached");
}
