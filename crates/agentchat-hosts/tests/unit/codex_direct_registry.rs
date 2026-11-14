//! Real kernel/metadata fences for simulated direct native associations.
use super::*;
use crate::device_metadata;
use crate::{claude::process::ProcessIdentity, launch::LaunchOwners};
use agentchat_core::sequence::Sequence;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    process::{Child, Command},
    time::{Duration, Instant},
};

struct Owner(Child);

#[test]
fn device_renumbering_keeps_live_owner_fenced_and_allows_fresh_owner_after_loss() {
    let mut f = Fixture::new();
    let registry = f.registry();
    let old = registry.publish(&f.binding).unwrap();
    device_metadata::renumber(&f.home);
    let original = fs::read(f.path()).unwrap();
    assert_eq!(registry.observe(&f.binding.context).unwrap(), old);
    assert!(
        registry
            .check_available(&f.binding.context.host_session_id)
            .is_err()
    );
    let successor = Owner::new();
    let mut fresh = f.refreshed();
    fresh.identity = LaunchOwners::capture(successor.0.id(), std::process::id()).unwrap();
    assert!(registry.publish(&fresh).is_err());
    assert_eq!(fs::read(f.path()).unwrap(), original);
    f.owner.stop();
    registry
        .check_available(&f.binding.context.host_session_id)
        .unwrap();
    let current = registry.publish(&fresh).unwrap();
    assert_eq!(current.revision(), old.revision() + 1);
    assert_eq!(registry.observe(&fresh.context).unwrap(), current);
    assert!(registry.observe(&f.binding.context).is_err());
}
impl Owner {
    fn new() -> Self {
        let child = Self(Command::new("/bin/sleep").arg("60").spawn().unwrap());
        let image = fs::canonicalize("/bin/sleep").unwrap();
        let metadata = image.metadata().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ProcessIdentity::capture(child.0.id())
            .is_ok_and(|p| p.matches_executable(&image, metadata.clone()))
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        child
    }
    fn stop(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    _temporary: tempfile::TempDir,
    home: PathBuf,
    owner: Owner,
    binding: Binding,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("state");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let owner = Owner::new();
        let context = Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            host: HostKind::Codex,
            host_session_id: Uuid::new_v4().to_string(),
            host_version: "0.164.7".into(),
            os_profile: "explicit-protocol-simulation".into(),
            working_directory: temporary.path().canonicalize().unwrap(),
        };
        let binding = Binding::new(
            context,
            LaunchOwners::capture(owner.0.id(), std::process::id()).unwrap(),
            Endpoint::Codex {
                socket: home.join("shared.sock"),
                executable: fs::canonicalize("/bin/sleep").unwrap(),
            },
        )
        .unwrap();
        Self {
            _temporary: temporary,
            home,
            owner,
            binding,
        }
    }
    fn registry(&self) -> NativeRegistry {
        NativeRegistry::new(&self.home)
    }
    fn path(&self) -> PathBuf {
        self.home
            .join("hosts/codex/roots")
            .join(format!("{}.json", self.binding.context.host_session_id))
    }
    fn refreshed(&self) -> Binding {
        let mut binding = self.binding.clone();
        binding.context.context_id = Uuid::new_v4();
        binding.context.incarnation = Uuid::new_v4();
        binding
    }
}

#[test]
fn absent_lookup_is_inert_and_exact_root_cannot_be_borrowed() {
    let temp = tempfile::tempdir().unwrap();
    let absent = temp.path().join("absent");
    assert!(
        NativeRegistry::new(&absent)
            .lookup(&Uuid::new_v4().to_string())
            .unwrap()
            .is_none()
    );
    assert!(!absent.exists());
    let f = Fixture::new();
    let route = f.registry().publish(&f.binding).unwrap();
    assert_eq!(route.version, 2);
    assert!(route.observer.is_none());
    assert_eq!(f.registry().observe(&f.binding.context).unwrap(), route);
    let mut other = f.binding.context.clone();
    other.host_session_id = Uuid::new_v4().to_string();
    assert!(f.registry().observe(&other).is_err());
    other = f.binding.context.clone();
    other.project_id = Uuid::new_v4();
    assert!(f.registry().observe(&other).is_err());
    assert!(f.registry().lookup(&Uuid::nil().to_string()).is_err());
}

#[test]
fn renewed_context_retains_scope_generation_and_original_start_boundary() {
    let f = Fixture::new();
    let registry = f.registry();
    let original = registry.publish(&f.binding).unwrap();
    assert_eq!(registry.publish(&f.binding).unwrap(), original);
    let starts = Registry::new(&f.home);
    assert!(!starts.was_started(&f.binding).unwrap());
    starts
        .mark_started(&f.binding, Sequence::new(7).unwrap())
        .unwrap();
    let marker = f
        .home
        .join("hosts/launches/started")
        .join(format!("{}.json", f.binding.identity.generation));
    let bytes = fs::read(&marker).unwrap();
    let fresh = f.refreshed();
    let next = registry.publish(&fresh).unwrap();
    assert_eq!(next.revision(), original.revision() + 1);
    assert_eq!(registry.publish(&fresh).unwrap(), next);
    assert!(registry.observe(&f.binding.context).is_err());
    assert_eq!(registry.observe(&fresh.context).unwrap(), next);
    assert!(starts.was_started(&fresh).unwrap());
    assert_eq!(fs::read(&marker).unwrap(), bytes);
    assert_eq!(starts.resolve(&f.binding.context).unwrap(), Some(f.binding));
}

#[test]
fn one_generation_cannot_repoint_its_project_directory_or_native_endpoint() {
    let f = Fixture::new();
    let registry = f.registry();
    let original = registry.publish(&f.binding).unwrap();
    let bytes = fs::read(f.path()).unwrap();
    for change in 0..4 {
        let mut replacement = f.refreshed();
        match change {
            0 => replacement.context.project_id = Uuid::new_v4(),
            1 => replacement.context.working_directory.push("other"),
            2 => replacement.context.host_version = "another-version".into(),
            _ => {
                replacement.endpoint = Endpoint::Codex {
                    socket: f.home.join("different.sock"),
                    executable: fs::canonicalize("/bin/sleep").unwrap(),
                }
            }
        }
        assert!(registry.publish(&replacement).is_err());
        assert_eq!(
            registry.lookup(&f.binding.context.host_session_id).unwrap(),
            Some(original.clone())
        );
        assert_eq!(fs::read(f.path()).unwrap(), bytes);
    }
}

#[test]
fn new_owner_requires_positive_loss_and_old_metadata_never_renews_it() {
    let mut f = Fixture::new();
    let original = f.registry().publish(&f.binding).unwrap();
    let second = Owner::new();
    let mut fresh = f.refreshed();
    fresh.identity = LaunchOwners::capture(second.0.id(), std::process::id()).unwrap();
    assert!(f.registry().publish(&fresh).is_err());
    f.owner.stop();
    assert!(
        f.registry()
            .lookup(&f.binding.context.host_session_id)
            .unwrap()
            .is_some()
    );
    assert!(f.registry().observe(&f.binding.context).is_err());
    assert!(f.registry().publish(&f.binding).is_err());
    let next = f.registry().publish(&fresh).unwrap();
    assert_eq!(next.revision(), original.revision() + 1);
    assert_eq!(f.registry().observe(&fresh.context).unwrap(), next);
}

#[test]
fn live_legacy_proxy_records_refuse_adoption_and_dead_ones_allow_fresh_launch() {
    let mut f = Fixture::new();
    let mut old = f.registry().publish(&f.binding).unwrap();
    old.version = 1;
    old.observer = Some(json!({"historical_observer":"not_used_for_authority"}));
    Publisher::new(&f.home).write(&f.path(), &old).unwrap();
    let bytes = fs::read(f.path()).unwrap();
    let error = f
        .registry()
        .lookup(&f.binding.context.host_session_id)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["reason"], "legacy_proxy_launch");
    assert_eq!(
        error.details["required_action"],
        "exit_managed_cli_then_fresh_managed_resume"
    );
    assert!(f.registry().publish(&f.binding).is_err());
    let second = Owner::new();
    let mut fresh = f.refreshed();
    fresh.identity = LaunchOwners::capture(second.0.id(), std::process::id()).unwrap();
    assert!(f.registry().publish(&fresh).is_err());
    assert_eq!(fs::read(f.path()).unwrap(), bytes);
    f.owner.stop();
    let current = f.registry().publish(&fresh).unwrap();
    assert_eq!(current.version, 2);
    assert_eq!(current.revision(), old.revision() + 1);
    assert!(current.observer.is_none());
    assert_eq!(
        Registry::new(&f.home).resolve(&f.binding.context).unwrap(),
        Some(f.binding)
    );
}

#[test]
fn a_failed_publication_can_retry_one_context_without_replacing_its_intent() {
    let f = Fixture::new();
    let original = f.registry().publish(&f.binding).unwrap();
    let lock_path = f
        .home
        .join("hosts/codex/roots")
        .join(format!("{}.lock", f.binding.context.host_session_id));
    let held = fs::File::open(lock_path).unwrap();
    fs2::FileExt::try_lock_exclusive(&held).unwrap();
    let fresh = f.refreshed();
    assert!(f.registry().publish(&fresh).is_err());
    assert_eq!(
        f.registry()
            .lookup(&f.binding.context.host_session_id)
            .unwrap(),
        Some(original.clone())
    );
    fs2::FileExt::unlock(&held).unwrap();
    let next = f.registry().publish(&fresh).unwrap();
    assert_eq!(next.binding(), &fresh);
    assert_eq!(next.revision(), original.revision() + 1);
    assert_eq!(f.registry().publish(&fresh).unwrap(), next);
}

#[test]
fn copied_or_modified_records_are_not_adopted_or_repaired() {
    let f = Fixture::new();
    f.registry().publish(&f.binding).unwrap();
    let changed = b"{\"version\":2}";
    fs::write(f.path(), changed).unwrap();
    assert!(
        f.registry()
            .lookup(&f.binding.context.host_session_id)
            .is_err()
    );
    assert!(f.registry().publish(&f.binding).is_err());
    assert_eq!(fs::read(f.path()).unwrap(), changed);
}
