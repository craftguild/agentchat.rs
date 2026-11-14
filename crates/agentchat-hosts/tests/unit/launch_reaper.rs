//! Actual kernel loss plus original/copy/edit/symlink proof preservation.
use super::*;
use crate::launch::Endpoint;
use agentchat_core::model::{Context, HostKind};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    process::{Child, Command},
};

struct Fixture {
    root: tempfile::TempDir,
    owner: Child,
    registry: Registry,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let owner = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let registry = Registry::new(root.path());
        Self {
            root,
            owner,
            registry,
        }
    }
    fn publish(&self) -> Binding {
        let executable = std::fs::canonicalize("/bin/sleep").unwrap();
        let metadata = std::fs::metadata(&executable).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Spawn may return before exec replaces the inherited test image. Keep
        // the intended image stable before testing native ownership fences.
        while !crate::claude::process::ProcessIdentity::capture(self.owner.id()).is_ok_and(
            |owner| owner.matches_executable(&executable, metadata.clone()) && owner.is_current(),
        ) {
            assert!(
                std::time::Instant::now() < deadline,
                "Owned sleep image did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let context = Context {
            context_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            host: HostKind::Claude,
            host_session_id: Uuid::new_v4().to_string(),
            host_version: "2.1.302".into(),
            os_profile: "test-only".into(),
            working_directory: self.root.path().into(),
        };
        let binding = Binding::new(
            context,
            LaunchOwners::capture(self.owner.id(), std::process::id()).unwrap(),
            Endpoint::Claude,
        )
        .unwrap();
        self.registry.publish(&binding).unwrap();
        // Explicit metadata fixture, not a claim that the test is a native
        // callback child. The SDK/helper integration exercises that producer.
        let directory = self
            .registry
            .data_directory("root-changes", true)
            .unwrap()
            .unwrap();
        metadata::Publisher::new(self.root.path())
            .create(
                &directory
                    .path
                    .join(format!("{}.json", binding.identity.generation)),
                &json!({"version":1,"binding":binding,"new_root":Uuid::new_v4()}),
            )
            .unwrap();
        binding
    }
    fn lost(&mut self) {
        self.owner.kill().unwrap();
        self.owner.wait().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.owner.kill();
        let _ = self.owner.wait();
    }
}
fn path(f: &Fixture, b: &Binding, kind: &str) -> PathBuf {
    let key = if kind == "bindings" {
        b.context.context_id
    } else {
        b.identity.generation
    };
    f.root
        .path()
        .join("hosts/launches")
        .join(kind)
        .join(format!("{key}.json"))
}
#[test]
fn removal_requires_original_managed_owner_loss_and_ignores_ordinary_or_other_project_processes() {
    let mut f = Fixture::new();
    // An ordinary live process without a managed proof is never a busy session.
    f.registry.require_quiescent(None).unwrap();
    let binding = f.publish();
    assert_eq!(
        f.registry
            .require_quiescent(Some(binding.context.project_id))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(
        f.registry.require_quiescent(None).unwrap_err().code,
        ErrorCode::Busy
    );
    f.registry.require_quiescent(Some(Uuid::new_v4())).unwrap();
    f.lost();
    f.registry
        .require_quiescent(Some(binding.context.project_id))
        .unwrap();
    f.registry.require_quiescent(None).unwrap();
}
#[test]
fn removal_preserves_uncertain_copied_or_edited_managed_metadata_even_after_owner_loss() {
    for variant in ["copy", "edit"] {
        let mut f = Fixture::new();
        let binding = f.publish();
        let target = path(&f, &binding, "bindings");
        let original = fs::read(&target).unwrap();
        if variant == "copy" {
            let temporary = f.root.path().join("copy");
            fs::write(&temporary, &original).unwrap();
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
            fs::rename(temporary, &target).unwrap();
        } else {
            let mut modified = original;
            modified.push(b' ');
            fs::write(&target, modified).unwrap();
        }
        f.lost();
        let bytes = fs::read(&target).unwrap();
        let error = f
            .registry
            .require_quiescent(Some(binding.context.project_id))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Busy);
        assert!(error.retryable);
        assert_eq!(fs::read(&target).unwrap(), bytes);
    }
}
#[test]
fn removal_of_another_project_ignores_a_proved_ready_scope_publication_lock() {
    let mut f = Fixture::new();
    let binding = f.publish();
    f.registry
        .publish_ready(&binding, f.root.path().join("control.sock"), None)
        .unwrap();
    let directory = f.registry.data_directory("ready", false).unwrap().unwrap();
    let key = crate::launch::readiness_scope::key(&binding.context).unwrap();
    let name = format!("{key}.lock");
    let snapshot = directory.read(name.as_ref()).unwrap().unwrap();
    let held = directory.lock_record(name.as_ref(), &snapshot).unwrap();
    f.registry.require_quiescent(Some(Uuid::new_v4())).unwrap();
    assert_eq!(
        f.registry
            .require_quiescent(Some(binding.context.project_id))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(
        f.registry.require_quiescent(None).unwrap_err().code,
        ErrorCode::Busy
    );
    drop(held);
    f.lost();
    f.registry.require_quiescent(None).unwrap();
}
#[test]
fn retained_pages_preserve_live_proofs_then_remove_only_after_positive_birth_loss() {
    let mut f = Fixture::new();
    let bindings = (0..21).map(|_| f.publish()).collect::<Vec<_>>();
    let mut reaper = LaunchReaper::new(f.root.path().into());
    for _ in 0..12 {
        let page = reaper.step().unwrap();
        assert!(page.visited <= 8);
        assert_eq!(page.removed, 0);
    }
    for b in &bindings {
        assert!(f.registry.root_changed(
            b.context.project_id,
            b.context.host,
            &b.context.host_session_id,
            &b.identity
        ));
        // A supervisor cannot fabricate a native root departure.
        assert!(f.registry.record_root_change(b, Uuid::new_v4()).is_err());
    }
    f.lost();
    let mut removed = 0;
    for _ in 0..24 {
        let page = reaper.step().unwrap();
        assert!(page.visited <= 8 && page.removed <= 8);
        removed += page.removed;
    }
    assert_eq!(removed, 42);
    for b in &bindings {
        assert!(!path(&f, b, "bindings").exists());
        assert!(!path(&f, b, "root-changes").exists());
        assert_eq!(LaunchOwners::observe(&b.identity), LaunchOwnerState::Gone);
    }
}
#[test]
fn foreign_records_are_preserved_and_never_become_root_departure_facts() {
    for kind in ["bindings", "root-changes"] {
        for variant in ["copy", "edit", "legacy", "symlink"] {
            let mut f = Fixture::new();
            let binding = f.publish();
            let target = path(&f, &binding, kind);
            let saved = target.with_extension("saved");
            fs::rename(&target, &saved).unwrap();
            match variant {
                "copy" => {
                    fs::copy(&saved, &target).unwrap();
                }
                "edit" => {
                    fs::rename(&saved, &target).unwrap();
                    let mut bytes = fs::read(&target).unwrap();
                    bytes.push(b' ');
                    fs::write(&target, bytes).unwrap();
                }
                "legacy" => {
                    fs::write(&target, serde_json::to_vec(&binding).unwrap()).unwrap();
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                }
                "symlink" => symlink(&saved, &target).unwrap(),
                _ => unreachable!(),
            }
            let bytes = fs::read(&target).unwrap();
            if kind == "root-changes" {
                assert!(!f.registry.root_changed(
                    binding.context.project_id,
                    binding.context.host,
                    &binding.context.host_session_id,
                    &binding.identity
                ));
            }
            f.lost();
            let mut reaper = LaunchReaper::new(f.root.path().into());
            for _ in 0..8 {
                let page = reaper.step().unwrap();
                assert!(page.visited <= 8 && page.removed <= 8);
            }
            assert_eq!(fs::read(&target).unwrap(), bytes, "{kind}/{variant}");
        }
    }
}
#[test]
fn missing_roots_are_inert_and_a_retained_directory_cannot_be_substituted() {
    let mut f = Fixture::new();
    let absent = f.root.path().join("absent");
    assert_eq!(LaunchReaper::new(absent.clone()).step().unwrap().visited, 0);
    assert!(!absent.exists());
    let binding = f.publish();
    let mut reaper = LaunchReaper::new(f.root.path().into());
    assert_eq!(reaper.step().unwrap().removed, 0);
    let directory = path(&f, &binding, "bindings")
        .parent()
        .unwrap()
        .to_path_buf();
    fs::rename(&directory, directory.with_extension("original")).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let foreign = directory.join("foreign");
    fs::write(&foreign, b"preserve").unwrap();
    f.lost();
    // Cycle through root changes, historical startup, physical readiness and profiles
    // before the retained bindings directory is revisited.
    for _ in 0..4 {
        reaper.step().unwrap();
    }
    assert!(reaper.step().is_err());
    assert_eq!(fs::read(foreign).unwrap(), b"preserve");
}

#[tokio::test]
async fn managed_profile_collection_requires_original_native_birth_loss_and_provenance() {
    for foreign_copy in [false, true] {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let executable = fs::canonicalize("/bin/sleep").unwrap();
        let mut child = crate::launch::NativeChild::spawn_with_startup_gate(
            &executable,
            root.path(),
            &["60".into()],
            &std::collections::BTreeMap::new(),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        let registry = Registry::new(root.path());
        registry
            .publish_claude_profile(
                &child,
                &crate::claude::observer::NativeProfile {
                    executable,
                    version: "test-only".into(),
                },
            )
            .unwrap();
        let path = root
            .path()
            .join("hosts/launches/claude-profiles")
            .join(format!("{}.json", child.identity().generation));
        let mut reaper = LaunchReaper::new(root.path().into());
        for _ in 0..5 {
            assert_eq!(reaper.step().unwrap().removed, 0);
        }
        assert!(path.is_file());
        if foreign_copy {
            let copy = path.with_extension("copied");
            fs::copy(&path, &copy).unwrap();
            fs::rename(copy, &path).unwrap();
        }
        child.terminate().await.unwrap();
        for _ in 0..10 {
            reaper.step().unwrap();
        }
        assert_eq!(path.exists(), foreign_copy);
    }
}

#[test]
fn completed_barrier_survives_supervisor_loss_until_original_native_birth_is_gone() {
    let mut f = Fixture::new();
    let mut b = f.publish();
    let mut supervisor = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let executable = std::fs::canonicalize("/bin/sleep").unwrap();
    let metadata = std::fs::metadata(&executable).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    // Spawn may return before exec replaces the inherited test image. Keep
    // the intended image stable before testing native ownership fences.
    while !crate::claude::process::ProcessIdentity::capture(supervisor.id()).is_ok_and(|owner| {
        owner.matches_executable(&executable, metadata.clone()) && owner.is_current()
    }) {
        assert!(
            std::time::Instant::now() < deadline,
            "Owned sleep image did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    b.identity = LaunchOwners::capture(f.owner.id(), supervisor.id()).unwrap();
    let directory = f.registry.data_directory("ready", true).unwrap().unwrap();
    let target = directory
        .path
        .join(format!("{}.json", b.identity.generation));
    let owner = crate::claude::process::ProcessIdentity::capture(f.owner.id()).unwrap();
    let owner_key =
        crate::launch::readiness::owner_key(&owner, &b.context.host_session_id).unwrap();
    let value = json!({"version":1,"binding":b,"control":"/tmp/synthetic-control.sock","preferred_name":null});
    // Reader/collector fixture with original-file proofs for both readiness
    // indexes. This grants no live control endpoint or participant.
    for path in [&target, &directory.path.join(format!("{owner_key}.json"))] {
        metadata::Publisher::new(f.root.path())
            .create(path, &value)
            .unwrap();
    }
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    assert_eq!(LaunchOwners::observe(&b.identity), LaunchOwnerState::Gone);
    let scope = crate::launch::StartupScope {
        root: Uuid::parse_str(&b.context.host_session_id).unwrap(),
        generation: b.identity.generation,
    };
    let owner = crate::claude::process::ProcessIdentity::capture(f.owner.id()).unwrap();
    assert_eq!(
        f.registry
            .startup_state(
                &scope,
                &owner,
                scope.root,
                &b.context.working_directory,
                &b.context.host_version
            )
            .unwrap(),
        crate::launch::StartupState::Waiting
    );
    let bytes = fs::read(&target).unwrap();
    let mut reaper = LaunchReaper::new(f.root.path().into());
    for _ in 0..12 {
        assert_eq!(reaper.step().unwrap().removed, 0);
    }
    assert_eq!(fs::read(&target).unwrap(), bytes);
    f.lost();
    for _ in 0..18 {
        let page = reaper.step().unwrap();
        assert!(page.visited <= 8 && page.removed <= 8);
    }
    assert!(!target.exists());
}
