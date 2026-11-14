//! An activation IPC fault is not permission to terminate ordinary native work.
use super::*;
use crate::{
    context::{ContextRegistry, ContextSeed},
    operations::{Launcher, activation::Native},
    project::{InitRequest, Project, ProjectManager},
};
use agentchat_core::{Error, model::HostKind, ports::Storage};
use agentchat_hosts::launch::{
    ActivationSocket, Binding, Endpoint, NativeProof, ReadyAuthorization, Registry,
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::net::UnixListener;
use uuid::Uuid;

struct NoRuntime;
#[async_trait::async_trait]
impl Launcher for NoRuntime {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        panic!("An unjoined native supervisor must not start a runtime");
    }
}
struct Report {
    proof: NativeProof,
    path: PathBuf,
    listener: Mutex<Option<UnixListener>>,
    survived: AtomicBool,
}
impl LaunchEvents for Report {
    fn authorized(&self, _: &NativeChild, ready: &ReadyAuthorization) -> Result<()> {
        assert_eq!(ready.control(), self.path);
        fs::rename(&self.path, self.path.with_extension("original")).unwrap();
        let listener = UnixListener::bind(&self.path).unwrap();
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600)).unwrap();
        *self.listener.lock().unwrap() = Some(listener);
        Ok(())
    }
    fn connection_lost(&self, _: &Error) -> Result<()> {
        assert_eq!(self.proof.state(), LaunchOwnerState::Current);
        self.survived.store(true, Ordering::SeqCst);
        // Test-only cleanup signals exactly the still-owned, unreaped child.
        // The supervisor must then observe its exit rather than aborting it as
        // a consequence of this unrelated activation socket replacement.
        let pid = i32::try_from(self.proof.owner().pid())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
        Ok(())
    }
}
#[tokio::test]
async fn endpoint_replacement_preserves_the_native_child_until_explicit_owned_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("project");
    fs::create_dir(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let manager = ProjectManager::new(root.path().join("state")).unwrap();
    let project = manager
        .initialize(
            &directory,
            InitRequest {
                expected_project_id: None,
                request_id: Uuid::new_v4(),
                scope: None,
                new_project: false,
                relocate: false,
                default_channels: None,
                receipt_settings: Default::default(),
                actor: Default::default(),
            },
        )
        .await
        .unwrap()
        .project;
    let native = fs::canonicalize("/bin/sleep").unwrap();
    let child = NativeChild::spawn(&native, &directory, &["30".into()], &BTreeMap::new())
        .await
        .unwrap();
    let proof = child.proof();
    let context = ContextRegistry::new(manager.clone())
        .issue(ContextSeed {
            host: HostKind::Codex,
            host_session_id: Uuid::new_v4().to_string(),
            host_version: "synthetic-supervision".into(),
            os_profile: "explicit-test-only".into(),
            working_directory: directory.clone(),
        })
        .await
        .unwrap();
    let binding = Binding::new(
        context,
        proof.identity().clone(),
        Endpoint::Codex {
            socket: root.path().join("unused-native.sock"),
            executable: native,
        },
    )
    .unwrap();
    let socket = ActivationSocket::bind(manager.home(), &binding).unwrap();
    let path = socket.path().to_owned();
    let registry = Registry::new(manager.home());
    registry.publish(&binding).unwrap();
    let ready = registry
        .publish_ready(&binding, path.clone(), None)
        .unwrap();
    let control = Control::new(socket, ready, Native::Inert);
    let operations = Operations::new(manager.clone(), Arc::new(NoRuntime));
    let report = Report {
        proof,
        path: path.clone(),
        listener: Mutex::new(None),
        survived: AtomicBool::new(false),
    };
    let route = Route {
        project: project.clone(),
        directory,
        context: None,
        user: false,
    };
    let code = tokio::time::timeout(
        Duration::from_secs(5),
        operations.supervise_ready(
            &route,
            child,
            Box::pin(async move { Ok(control) }),
            &report,
            signals().unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(code, 143);
    assert!(report.survived.load(Ordering::SeqCst));
    let store = manager.open(&project).await.unwrap();
    assert_eq!(
        store
            .snapshot()
            .await
            .unwrap()
            .statistics()
            .await
            .unwrap()
            .agents,
        0
    );
    report.listener.lock().unwrap().take();
    fs::remove_file(&path).unwrap();
    fs::remove_file(path.with_extension("original")).unwrap();
    fs::remove_dir(path.parent().unwrap()).unwrap();
}
