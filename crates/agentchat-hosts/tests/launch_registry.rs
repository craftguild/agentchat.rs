//! Immutable supervisor publication is separate from liveness and admission.
use agentchat_core::model::{Context, HostKind};
use agentchat_hosts::launch::{Binding, Endpoint, LaunchOwners, Registry};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;
struct Owned(Child);
impl Owned {
    fn new() -> Self {
        let executable = fs::canonicalize("/bin/sleep").unwrap();
        let metadata = fs::metadata(&executable).unwrap();
        let child = Self(Command::new(&executable).arg("60").spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        // A spawned Linux child can still expose its inherited pre-exec image.
        // Pin the intended image before testing immutable supervisor records;
        // production NativeChild already waits for this exact-image boundary.
        while !agentchat_hosts::claude::process::ProcessIdentity::capture(child.0.id()).is_ok_and(
            |owner| owner.matches_executable(&executable, metadata.clone()) && owner.is_current(),
        ) {
            assert!(Instant::now() < deadline, "Owned sleep image did not start");
            std::thread::sleep(Duration::from_millis(1));
        }
        child
    }
    fn binding(&self, context: Context) -> Binding {
        Binding::new(
            context,
            LaunchOwners::capture(self.0.id(), std::process::id()).unwrap(),
            Endpoint::Claude,
        )
        .unwrap()
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
fn root() -> TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn context() -> Context {
    Context {
        context_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        host: HostKind::Claude,
        host_session_id: Uuid::new_v4().to_string(),
        host_version: "synthetic-only".into(),
        os_profile: "synthetic-test".into(),
        working_directory: PathBuf::from("/tmp"),
        incarnation: Uuid::new_v4(),
    }
}
#[test]
fn startup_release_is_immutable_exact_child_scoped_and_not_live_participation() {
    use agentchat_core::sequence::Sequence;
    use agentchat_hosts::{
        claude::process::ProcessIdentity,
        launch::{StartupScope, StartupState},
    };
    let root = root();
    let owner = Owned::new();
    let binding = owner.binding(context());
    let registry = Registry::new(root.path());
    let physical = ProcessIdentity::capture(owner.0.id()).unwrap();
    let scope = StartupScope {
        root: Uuid::parse_str(&binding.context.host_session_id).unwrap(),
        generation: binding.identity.generation,
    };
    let check = |root, cwd: &std::path::Path, version: &str| {
        registry.startup_state(&scope, &physical, root, cwd, version)
    };
    assert_eq!(
        check(
            scope.root,
            &binding.context.working_directory,
            &binding.context.host_version
        )
        .unwrap(),
        StartupState::Waiting
    );
    assert!(!root.path().join("hosts").exists());
    assert!(
        registry
            .mark_started(&binding, Sequence::new(7).unwrap())
            .is_err()
    );
    registry.publish(&binding).unwrap();
    registry
        .mark_started(&binding, Sequence::new(7).unwrap())
        .unwrap();
    let path = root
        .path()
        .join("hosts/launches/started")
        .join(format!("{}.json", scope.generation));
    let bytes = fs::read(&path).unwrap();
    registry
        .mark_started(&binding, Sequence::new(7).unwrap())
        .unwrap();
    assert!(
        registry
            .mark_started(&binding, Sequence::new(8).unwrap())
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    // A historical membership marker no longer releases the input barrier.
    assert_eq!(
        check(
            scope.root,
            &binding.context.working_directory,
            &binding.context.host_version
        )
        .unwrap(),
        StartupState::Waiting
    );
    registry
        .publish_ready(&binding, "/tmp/synthetic-launch-control.sock".into(), None)
        .unwrap();
    let path = root
        .path()
        .join("hosts/launches/ready")
        .join(format!("{}.json", scope.generation));
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        check(
            scope.root,
            &binding.context.working_directory,
            &binding.context.host_version
        )
        .unwrap(),
        StartupState::Released
    );
    assert!(
        check(
            scope.root,
            std::path::Path::new("/different"),
            &binding.context.host_version
        )
        .is_err()
    );
    assert!(
        check(
            scope.root,
            &binding.context.working_directory,
            "wrong-version"
        )
        .is_err()
    );
    assert_eq!(
        check(
            Uuid::new_v4(),
            &binding.context.working_directory,
            &binding.context.host_version
        )
        .unwrap(),
        StartupState::OtherRoot
    );
    let other = Owned::new();
    assert_eq!(
        registry
            .startup_state(
                &scope,
                &ProcessIdentity::capture(other.0.id()).unwrap(),
                scope.root,
                &binding.context.working_directory,
                &binding.context.host_version
            )
            .unwrap(),
        StartupState::OtherOwner
    );
    fs::rename(&path, path.with_extension("original")).unwrap();
    fs::copy(path.with_extension("original"), &path).unwrap();
    assert!(
        check(
            scope.root,
            &binding.context.working_directory,
            &binding.context.host_version
        )
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
}
fn path(root: &TempDir, context: &Context) -> PathBuf {
    root.path()
        .join("hosts/launches/bindings")
        .join(format!("{}.json", context.context_id))
}
#[test]
fn lookup_is_inert_and_supervisor_publication_is_immutable_and_exact() {
    let root = root();
    let absent = root.path().join("absent");
    assert!(
        Registry::new(&absent)
            .resolve(&context())
            .unwrap()
            .is_none()
    );
    assert!(!absent.exists());
    let owner = Owned::new();
    let binding = owner.binding(context());
    let registry = Registry::new(root.path());
    assert!(registry.resolve(&binding.context).unwrap().is_none());
    assert!(!root.path().join("hosts").exists());
    registry.publish(&binding).unwrap();
    let before = fs::read(path(&root, &binding.context)).unwrap();
    registry.publish(&binding).unwrap();
    assert_eq!(fs::read(path(&root, &binding.context)).unwrap(), before);
    assert_eq!(
        registry.resolve(&binding.context).unwrap(),
        Some(binding.clone())
    );
    let mut wrong_root = binding.context.clone();
    wrong_root.host_session_id = "another-root".into();
    let mut wrong_project = binding.context.clone();
    wrong_project.project_id = Uuid::new_v4();
    let mut wrong_incarnation = binding.context.clone();
    wrong_incarnation.incarnation = Uuid::new_v4();
    for other in [wrong_root, wrong_project, wrong_incarnation] {
        assert!(registry.resolve(&other).is_err());
    }
    let mut other = binding.clone();
    other.identity.generation = Uuid::new_v4();
    assert!(registry.publish(&other).is_err());
    assert_eq!(fs::read(path(&root, &binding.context)).unwrap(), before);
}
#[test]
fn lost_original_owner_keeps_readable_facts_without_permitting_republication_or_revival() {
    let root = root();
    let mut owner = Owned::new();
    let binding = owner.binding(context());
    let registry = Registry::new(root.path());
    registry.publish(&binding).unwrap();
    owner.kill();
    assert_eq!(
        registry.resolve(&binding.context).unwrap(),
        Some(binding.clone())
    );
    assert!(registry.publish(&binding).is_err());
    let successor = Owned::new();
    let next = successor.binding(binding.context.clone());
    assert!(registry.publish(&next).is_err());
    assert_eq!(registry.resolve(&binding.context).unwrap(), Some(binding));
}
#[test]
fn copied_edited_legacy_and_symbolic_records_are_preserved_without_adoption() {
    for variant in ["copied", "edited", "legacy", "symlink"] {
        let root = root();
        let owner = Owned::new();
        let binding = owner.binding(context());
        let registry = Registry::new(root.path());
        registry.publish(&binding).unwrap();
        let record = path(&root, &binding.context);
        let saved = record.with_extension("original");
        fs::rename(&record, &saved).unwrap();
        match variant {
            "copied" => {
                fs::copy(&saved, &record).unwrap();
            }
            "edited" => {
                fs::rename(&saved, &record).unwrap();
                let mut bytes = fs::read(&record).unwrap();
                bytes.push(b' ');
                fs::write(&record, bytes).unwrap();
            }
            "legacy" => {
                fs::write(&record, serde_json::to_vec(&binding).unwrap()).unwrap();
                fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
            }
            "symlink" => symlink(&saved, &record).unwrap(),
            _ => unreachable!(),
        }
        let before = fs::read(&record).unwrap();
        assert!(registry.resolve(&binding.context).is_err(), "{variant}");
        assert!(registry.publish(&binding).is_err(), "{variant}");
        assert_eq!(fs::read(&record).unwrap(), before, "{variant}");
    }
}
#[test]
#[ignore = "owned child; the supervisor publication parent invokes this helper"]
fn helper_cannot_publish_for_original_supervisor() {
    let binding: Binding =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_LAUNCH_BINDING").unwrap()).unwrap();
    let home = std::env::var_os("AGENTCHAT_TEST_LAUNCH_HOME").unwrap();
    let registry = Registry::new(home);
    assert!(registry.publish(&binding).is_err());
    assert!(
        registry
            .mark_started(
                &binding,
                agentchat_core::sequence::Sequence::new(7).unwrap()
            )
            .is_err()
    );
}
#[test]
fn a_helper_with_copied_tokens_cannot_create_a_binding_for_its_parent() {
    let root = root();
    let owner = Owned::new();
    let binding = owner.binding(context());
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "helper_cannot_publish_for_original_supervisor",
        ])
        .env(
            "AGENTCHAT_TEST_LAUNCH_BINDING",
            serde_json::to_string(&binding).unwrap(),
        )
        .env("AGENTCHAT_TEST_LAUNCH_HOME", root.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!root.path().join("hosts").exists());
}
