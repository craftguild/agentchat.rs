//! Physical readiness is independent from membership generations and PID copies.
#[path = "readiness/fixture.rs"]
mod fixture;
use agentchat_core::{ports::HostGate, sequence::Sequence};
use agentchat_hosts::{
    AdmittedHosts,
    claude::process::ProcessIdentity,
    launch::{ActivationSocket, Binding, Endpoint, Registry, StartupScope},
};
use fixture::Fixture;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use uuid::Uuid;

#[tokio::test]
async fn ready_is_inert_then_retained_across_same_owner_participation_generations() {
    let mut f = Fixture::new();
    let registry = Registry::new(f.home.path());
    assert!(registry.readiness(&f.binding).unwrap().is_none());
    assert!(!f.home.path().join("hosts").exists());
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    assert!(socket.path().as_os_str().as_encoded_bytes().len() < 100);
    assert!(
        registry
            .publish_ready(&f.binding, socket.path().into(), Some("steady-fox".into()))
            .is_err()
    );
    registry.publish(&f.binding).unwrap();
    let ready = registry
        .publish_ready(&f.binding, socket.path().into(), Some("steady-fox".into()))
        .unwrap();
    assert_eq!(ready.preferred_name(), Some("steady-fox"));
    assert!(ready.supervisor_peer(std::process::id()));
    assert!(!ready.supervisor_peer(f.child.id()));
    assert_eq!(
        registry
            .publish_ready(&f.binding, socket.path().into(), Some("steady-fox".into()))
            .unwrap(),
        ready
    );
    let mut next = f.binding.clone();
    next.identity.generation = Uuid::new_v4();
    next.context.context_id = Uuid::new_v4();
    next.context.incarnation = Uuid::new_v4();
    registry.publish(&next).unwrap();
    assert_eq!(registry.readiness(&next).unwrap(), Some(ready.clone()));
    let mut foreign = next.clone();
    foreign.context.host_session_id = Uuid::new_v4().to_string();
    assert!(registry.readiness(&foreign).unwrap().is_none());
    foreign = next.clone();
    foreign.context.project_id = Uuid::new_v4();
    assert!(registry.readiness(&foreign).is_err());
    f.stop();
    assert_eq!(registry.readiness(&next).unwrap(), Some(ready.clone()));
    assert!(!ready.supervisor_peer(std::process::id()));
    assert!(
        registry
            .publish_ready(&f.binding, socket.path().into(), Some("steady-fox".into()))
            .is_err()
    );
}

#[tokio::test]
async fn delivery_identity_waits_for_the_exact_committed_start_marker() {
    let f = Fixture::new();
    let registry = Registry::new(f.home.path());
    registry.publish(&f.binding).unwrap();
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    registry
        .publish_ready(&f.binding, socket.path().into(), None)
        .unwrap();
    let gate = AdmittedHosts::new(f.home.path().into());
    assert_eq!(
        gate.launch_identity(&f.binding.context).await.unwrap(),
        Some(f.binding.identity.clone())
    );
    assert_eq!(
        gate.launch_delivery_identity(&f.binding.context)
            .await
            .unwrap(),
        None
    );
    registry
        .mark_started(&f.binding, Sequence::new(1).unwrap())
        .unwrap();
    assert_eq!(
        gate.launch_delivery_identity(&f.binding.context)
            .await
            .unwrap(),
        Some(f.binding.identity.clone())
    );
}

#[tokio::test]
async fn started_same_root_cannot_become_a_pending_managed_activation() {
    let f = Fixture::new();
    let registry = Registry::new(f.home.path());
    registry.publish(&f.binding).unwrap();
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    registry
        .publish_ready(&f.binding, socket.path().into(), None)
        .unwrap();
    registry
        .mark_started(&f.binding, Sequence::new(1).unwrap())
        .unwrap();
    let scope = StartupScope {
        root: Uuid::parse_str(&f.binding.context.host_session_id).unwrap(),
        generation: f.binding.identity.generation,
    };
    let owner = ProcessIdentity::capture(f.child.id()).unwrap();
    assert_eq!(
        registry
            .startup_state(
                &scope,
                &owner,
                scope.root,
                &f.binding.context.working_directory,
                &f.binding.context.host_version,
            )
            .unwrap(),
        agentchat_hosts::launch::StartupState::Released
    );
    let mut same_root = f.binding.context.clone();
    same_root.context_id = Uuid::new_v4();
    same_root.incarnation = Uuid::new_v4();
    // Membership is intentionally outside this host predicate. Therefore the
    // false result also holds after explicit Leave, and Restoration must treat
    // its resulting NotJoined as terminal rather than waiting for auto-Join.
    assert!(
        !registry
            .managed_transition_pending(&scope, &owner, &same_root)
            .unwrap()
    );
}

#[tokio::test]
async fn copied_ready_index_is_not_adopted_or_replaced() {
    let f = Fixture::new();
    let registry = Registry::new(f.home.path());
    registry.publish(&f.binding).unwrap();
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    registry
        .publish_ready(&f.binding, socket.path().into(), None)
        .unwrap();
    let path = f
        .home
        .path()
        .join("hosts/launches/ready")
        .join(format!("{}.json", f.binding.identity.generation));
    let before = fs::read(&path).unwrap();
    fs::rename(&path, path.with_extension("original")).unwrap();
    fs::copy(path.with_extension("original"), &path).unwrap();
    assert!(registry.readiness(&f.binding).is_err());
    assert!(
        registry
            .publish_ready(&f.binding, socket.path().into(), None)
            .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[tokio::test]
async fn endpoint_replacement_is_preserved_by_old_owner_drop() {
    let f = Fixture::new();
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    let path = socket.path().to_owned();
    let original = path.with_extension("original");
    fs::rename(&path, &original).unwrap();
    let replacement = tokio::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(socket.accept().await.is_err());
    drop(socket);
    assert!(path.exists());
    drop(replacement);
    fs::remove_file(&path).unwrap();
    fs::remove_file(original).unwrap();
    fs::remove_dir(path.parent().unwrap()).unwrap();
}

#[test]
#[ignore = "owned helper invoked by copied_token_helper_cannot_publish_readiness"]
fn copied_token_helper() {
    let binding = serde_json::from_str(&std::env::var("AGENTCHAT_READY_BINDING").unwrap()).unwrap();
    let home = std::env::var_os("AGENTCHAT_READY_HOME").unwrap();
    let registry = Registry::new(&home);
    assert!(
        registry
            .publish_ready(&binding, "/tmp/not-a-real-activation.sock".into(), None)
            .is_err()
    );
    assert!(ActivationSocket::bind(std::path::Path::new(&home), &binding).is_err());
}

#[tokio::test]
async fn copied_token_helper_cannot_publish_readiness() {
    let f = Fixture::new();
    let registry = Registry::new(f.home.path());
    registry.publish(&f.binding).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "copied_token_helper"])
        .env(
            "AGENTCHAT_READY_BINDING",
            serde_json::to_string(&f.binding).unwrap(),
        )
        .env("AGENTCHAT_READY_HOME", f.home.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!f.home.path().join("hosts/launches/ready").exists());
}

#[tokio::test]
async fn conversation_lookup_routes_a_fresh_unbound_descriptor_without_publishing_or_joining_it() {
    let f = Fixture::new();
    let registry = Registry::new(f.home.path());
    registry.publish(&f.binding).unwrap();
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    let ready = registry
        .publish_ready(&f.binding, socket.path().into(), None)
        .unwrap();
    let mut refreshed = f.binding.context.clone();
    refreshed.context_id = Uuid::new_v4();
    refreshed.incarnation = Uuid::new_v4();
    assert!(registry.resolve(&refreshed).unwrap().is_none());
    assert_eq!(registry.authorization(&refreshed).unwrap(), Some(ready));
    assert!(registry.resolve(&refreshed).unwrap().is_none());
    let mut foreign = refreshed.clone();
    foreign.host_session_id = Uuid::new_v4().to_string();
    assert!(registry.authorization(&foreign).unwrap().is_none());
    foreign = refreshed;
    foreign.working_directory = f.home.path().join("another-directory");
    assert!(registry.authorization(&foreign).is_err());
}

#[tokio::test]
async fn a_second_physical_owner_cannot_replace_conversation_authorization_until_positive_loss() {
    let mut original = Fixture::new();
    let registry = Registry::new(original.home.path());
    registry.publish(&original.binding).unwrap();
    let socket = ActivationSocket::bind(original.home.path(), &original.binding).unwrap();
    let ready = registry
        .publish_ready(&original.binding, socket.path().into(), None)
        .unwrap();
    let mut successor = Fixture::new();
    successor.binding.context = original.binding.context.clone();
    successor.binding.context.context_id = Uuid::new_v4();
    successor.binding.context.incarnation = Uuid::new_v4();
    registry.publish(&successor.binding).unwrap();
    let next_socket = ActivationSocket::bind(original.home.path(), &successor.binding).unwrap();
    assert!(
        registry
            .publish_ready(&successor.binding, next_socket.path().into(), None)
            .is_err()
    );
    assert_eq!(
        registry.authorization(&successor.binding.context).unwrap(),
        Some(ready)
    );
    original.stop();
    let fresh = registry
        .publish_ready(&successor.binding, next_socket.path().into(), None)
        .unwrap();
    assert_eq!(
        registry.authorization(&successor.binding.context).unwrap(),
        Some(fresh.clone())
    );
    assert_eq!(
        registry.authorization(&original.binding.context).unwrap(),
        Some(fresh)
    );
}

#[tokio::test]
async fn a_copied_conversation_index_is_preserved_and_never_bypasses_original_file_proof() {
    let f = Fixture::new();
    let registry = Registry::new(f.home.path());
    registry.publish(&f.binding).unwrap();
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    registry
        .publish_ready(&f.binding, socket.path().into(), None)
        .unwrap();
    let path = fs::read_dir(f.home.path().join("hosts/launches/ready"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("scope-")
                && path.extension().is_some_and(|e| e == "json")
        })
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    let old = path.with_extension("original");
    fs::rename(&path, &old).unwrap();
    fs::copy(&old, &path).unwrap();
    assert!(registry.authorization(&f.binding.context).is_err());
    assert!(
        registry
            .publish_ready(&f.binding, socket.path().into(), None)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[tokio::test]
async fn positive_root_departure_fences_only_the_exact_participation_generation() {
    let f = Fixture::new();
    let registry = Registry::new(f.home.path());
    registry.publish(&f.binding).unwrap();
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    registry
        .publish_ready(&f.binding, socket.path().into(), None)
        .unwrap();
    let directory = f.home.path().join("hosts/launches/root-changes");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    agentchat_hosts::claude::metadata::Publisher::new(f.home.path())
        .create(
            &directory.join(format!("{}.json", f.binding.identity.generation)),
            &serde_json::json!({"version":1,"binding":f.binding,"new_root":Uuid::new_v4()}),
        )
        .unwrap();
    let mut later = f.binding.clone();
    later.identity.generation = Uuid::new_v4();
    later.context.context_id = Uuid::new_v4();
    later.context.incarnation = Uuid::new_v4();
    registry.publish(&later).unwrap();
    assert!(registry.root_changed(
        later.context.project_id,
        later.context.host,
        &later.context.host_session_id,
        &f.binding.identity
    ));
    assert!(!registry.root_changed(
        later.context.project_id,
        later.context.host,
        &later.context.host_session_id,
        &later.identity
    ));
    let other = Fixture::new();
    assert!(!registry.root_changed(
        later.context.project_id,
        later.context.host,
        &later.context.host_session_id,
        &other.binding.identity
    ));
}

#[tokio::test]
async fn correlated_return_replaces_scope_with_a_fresh_unfenced_generation() {
    let f = Fixture::new();
    let registry = Registry::new(f.home.path());
    registry.publish(&f.binding).unwrap();
    let socket = ActivationSocket::bind(f.home.path(), &f.binding).unwrap();
    registry
        .publish_ready(&f.binding, socket.path().into(), None)
        .unwrap();
    let changes = f.home.path().join("hosts/launches/root-changes");
    fs::create_dir(&changes).unwrap();
    fs::set_permissions(&changes, fs::Permissions::from_mode(0o700)).unwrap();
    let publisher = agentchat_hosts::claude::metadata::Publisher::new(f.home.path());
    let second_root = Uuid::new_v4();
    publisher
        .create(
            &changes.join(format!("{}.json", f.binding.identity.generation)),
            &serde_json::json!({"version":1,"binding":f.binding,"new_root":second_root}),
        )
        .unwrap();
    let scope = StartupScope {
        root: Uuid::parse_str(&f.binding.context.host_session_id).unwrap(),
        generation: f.binding.identity.generation,
    };
    let owner = ProcessIdentity::capture(f.child.id()).unwrap();
    let mut pending = f.binding.context.clone();
    pending.context_id = Uuid::new_v4();
    pending.incarnation = Uuid::new_v4();
    pending.host_session_id = second_root.to_string();
    assert!(
        registry
            .managed_transition_pending(&scope, &owner, &pending)
            .unwrap()
    );
    pending.project_id = Uuid::new_v4();
    assert!(
        !registry
            .managed_transition_pending(&scope, &owner, &pending)
            .unwrap()
    );

    let mut second = f.binding.clone();
    second.identity.generation = Uuid::new_v4();
    second.context.context_id = Uuid::new_v4();
    second.context.incarnation = Uuid::new_v4();
    second.context.host_session_id = second_root.to_string();
    registry.publish(&second).unwrap();
    registry
        .publish_ready(&second, socket.path().into(), None)
        .unwrap();
    publisher
        .create(
            &changes.join(format!("{}.json", second.identity.generation)),
            &serde_json::json!({"version":1,"binding":second,"new_root":f.binding.context.host_session_id}),
        )
        .unwrap();

    let mut returned = f.binding.clone();
    returned.identity.generation = Uuid::new_v4();
    returned.context.context_id = Uuid::new_v4();
    returned.context.incarnation = Uuid::new_v4();
    registry.publish(&returned).unwrap();
    let ready = registry
        .publish_ready(&returned, socket.path().into(), None)
        .unwrap();
    assert_eq!(
        registry.authorization(&returned.context).unwrap(),
        Some(ready)
    );
    assert!(registry.root_changed(
        f.binding.context.project_id,
        f.binding.context.host,
        &f.binding.context.host_session_id,
        &f.binding.identity
    ));
    assert!(!registry.root_changed(
        returned.context.project_id,
        returned.context.host,
        &returned.context.host_session_id,
        &returned.identity
    ));
}

#[tokio::test]
async fn codex_transition_fence_is_exact_reversible_or_promoted_to_departure() {
    let f = Fixture::new();
    let mut context = f.binding.context.clone();
    context.host = agentchat_core::model::HostKind::Codex;
    let binding = Binding::new(
        context,
        f.binding.identity.clone(),
        Endpoint::Codex {
            socket: f.home.path().join("synthetic-daemon.sock"),
            executable: fs::canonicalize("/bin/sleep").unwrap(),
        },
    )
    .unwrap();
    let registry = Registry::new(f.home.path());
    registry.publish(&binding).unwrap();
    let socket = ActivationSocket::bind(f.home.path(), &binding).unwrap();
    registry
        .publish_ready(&binding, socket.path().into(), None)
        .unwrap();

    registry.record_managed_transition(&binding, 1, 1).unwrap();
    assert!(registry.root_changed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    assert!(!registry.root_departed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    assert!(
        registry
            .rollback_managed_transition(&binding, 1, 2)
            .is_err()
    );
    assert!(registry.root_changed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    registry
        .rollback_managed_transition(&binding, 1, 1)
        .unwrap();
    assert!(!registry.root_changed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    assert!(!registry.root_departed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));

    let target = Uuid::new_v4();
    registry.record_managed_transition(&binding, 2, 3).unwrap();
    registry
        .record_managed_root_change(&binding, target, 2, 3)
        .unwrap();
    registry
        .record_managed_root_change(&binding, target, 2, 3)
        .unwrap();
    assert!(
        registry
            .record_managed_root_change(&binding, Uuid::new_v4(), 2, 3)
            .is_err()
    );
    assert!(registry.root_changed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    assert!(registry.root_departed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    assert!(
        registry
            .rollback_managed_transition(&binding, 2, 3)
            .is_err()
    );
}
