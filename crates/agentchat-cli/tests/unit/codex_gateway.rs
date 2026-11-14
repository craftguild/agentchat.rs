//! First-call races use real kernel/metadata proofs, without host API guesses.
use super::*;
use agentchat_core::model::Context;
use agentchat_hosts::{
    claude::metadata,
    launch::{Binding, Endpoint, LaunchOwners},
};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    process::{Child, Command},
    time::Instant,
};

struct Owner(Child);
impl Owner {
    fn new() -> Self {
        let child = Self(Command::new("/bin/sleep").arg("60").spawn().unwrap());
        let image = fs::canonicalize("/bin/sleep").unwrap();
        let stat = image.metadata().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ProcessIdentity::capture(child.0.id())
            .is_ok_and(|p| p.matches_executable(&image, stat.clone()))
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
    verifier: Verifier,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("state");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let manager = ProjectManager::new(home.clone()).unwrap();
        let backend = Arc::new(crate::process::operations(
            manager.clone(),
            std::env::current_exe().unwrap(),
        ));
        let owner = Owner::new();
        let context = Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            host: HostKind::Codex,
            host_session_id: Uuid::new_v4().to_string(),
            host_version: "simulated-native-version".into(),
            os_profile: "explicit-protocol-simulation".into(),
            working_directory: temporary.path().canonicalize().unwrap(),
        };
        let parent = ProcessIdentity::capture(std::process::id()).unwrap();
        let daemon = Daemon {
            socket: home.join("native.sock"),
            executable: fs::canonicalize("/bin/sleep").unwrap(),
            version: context.host_version.clone(),
        };
        let binding = Binding::new(
            context,
            LaunchOwners::capture(owner.0.id(), std::process::id()).unwrap(),
            Endpoint::Codex {
                socket: daemon.socket.clone(),
                executable: daemon.executable.clone(),
            },
        )
        .unwrap();
        let verifier = Verifier {
            parent,
            daemon,
            registry: NativeRegistry::new(&home),
            launches: Registry::new(&home),
            adapter: NativeAdapter::new(home.clone()),
            roots: Arc::new(Conversations::new(backend)),
            bootstrap: Uuid::new_v4(),
            state: Mutex::default(),
        };
        Self {
            _temporary: temporary,
            home,
            owner,
            binding,
            verifier,
        }
    }
}

#[tokio::test]
async fn first_call_waits_for_original_readiness_without_membership_or_a_native_resume_barrier() {
    let f = Fixture::new();
    f.verifier.registry.publish(&f.binding).unwrap();
    let wait = f
        .verifier
        .startup_route(Uuid::parse_str(&f.binding.context.host_session_id).unwrap());
    tokio::pin!(wait);
    tokio::select! {
        result=&mut wait=>panic!("Uncommitted startup became visible: {result:?}"),
        _=tokio::time::sleep(Duration::from_millis(40))=>{},
    }
    f.verifier
        .launches
        .publish_ready(&f.binding, "/tmp/synthetic-codex-control.sock".into(), None)
        .unwrap();
    let route = tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(route.binding(), &f.binding);
    assert!(!f.verifier.daemon.socket.exists());
}

#[tokio::test]
async fn exact_native_metadata_is_required_and_a_different_root_cannot_borrow_state() {
    let f = Fixture::new();
    assert_eq!(
        f.verifier
            .verify_scope(CallEvidence {
                tool_name: "agentchat_whoami",
                metadata: None
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    let root = Uuid::parse_str(&f.binding.context.host_session_id).unwrap();
    f.verifier.state.lock().await.root = Some(root);
    let metadata = serde_json::from_value(
        json!({"threadId":Uuid::new_v4(),"cwd":f.binding.context.working_directory}),
    )
    .unwrap();
    assert_eq!(
        f.verifier
            .verify_scope(CallEvidence {
                tool_name: "agentchat_whoami",
                metadata: Some(&metadata)
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    assert_eq!(f.verifier.state.lock().await.root, Some(root));
    assert!(
        f.verifier
            .registry
            .lookup(&root.to_string())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn native_parent_loss_while_waiting_does_not_publish_or_restore_participation() {
    let mut f = Fixture::new();
    let mut parent = Owner::new();
    f.verifier.parent = ProcessIdentity::capture(parent.0.id()).unwrap();
    f.verifier.registry.publish(&f.binding).unwrap();
    let wait = f
        .verifier
        .startup_route(Uuid::parse_str(&f.binding.context.host_session_id).unwrap());
    tokio::pin!(wait);
    tokio::select! {
        result=&mut wait=>panic!("Uncommitted startup became visible: {result:?}"),
        _=tokio::time::sleep(Duration::from_millis(40))=>{},
    }
    parent.stop();
    assert_eq!(wait.await.unwrap_err().code, ErrorCode::SessionRequired);
    assert!(f.verifier.launches.readiness(&f.binding).unwrap().is_none());
    assert!(
        f.verifier
            .registry
            .lookup(&f.binding.context.host_session_id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn retired_legacy_locator_waits_for_a_fresh_generation_and_never_becomes_v2_authority() {
    let mut f = Fixture::new();
    let old = f.verifier.registry.publish(&f.binding).unwrap();
    let mut legacy = serde_json::to_value(old).unwrap();
    legacy["version"] = json!(1);
    legacy["observer"] = json!({"historical":true});
    let path = f
        .home
        .join("hosts/codex/roots")
        .join(format!("{}.json", f.binding.context.host_session_id));
    metadata::write(&path, &legacy).unwrap();
    f.owner.stop();
    let second = Owner::new();
    let mut fresh = f.binding.clone();
    fresh.identity = LaunchOwners::capture(second.0.id(), std::process::id()).unwrap();
    fresh.context.context_id = Uuid::new_v4();
    fresh.context.incarnation = Uuid::new_v4();
    let wait = f
        .verifier
        .startup_route(Uuid::parse_str(&fresh.context.host_session_id).unwrap());
    tokio::pin!(wait);
    tokio::select! {
        result=&mut wait=>panic!("Legacy metadata was adopted or rejected before replacement: {result:?}"),
        _=tokio::time::sleep(Duration::from_millis(40))=>{},
    }
    f.verifier.registry.publish(&fresh).unwrap();
    f.verifier
        .launches
        .publish_ready(&fresh, "/tmp/synthetic-codex-control.sock".into(), None)
        .unwrap();
    let route = tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(route.binding(), &fresh);
    assert_ne!(
        route.binding().identity.generation,
        f.binding.identity.generation
    );
}
