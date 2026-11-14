use super::*;
use agentchat_core::application::Mutation;
use agentchat_hosts::launch::NativeProof;
use agentchat_protocol::rpc::{Call, success};

struct Fixture {
    _root: tempfile::TempDir,
    operations: Operations,
    route: crate::operations::Route,
    control: Control,
    original: Binding,
    proof: NativeProof,
    child: NativeChild,
    runtime: crate::daemon::RuntimeHandle,
    runtime_task: tokio::task::JoinHandle<Result<()>>,
    backend: tokio::task::JoinHandle<()>,
    go: std::path::PathBuf,
    seen: std::path::PathBuf,
    target: String,
}
impl Fixture {
    async fn new(same_root: bool) -> Self {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let directory = root.path().join("project");
        fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let manager = ProjectManager::new(root.path().join("state")).unwrap();
        let project = manager
            .initialize(&directory, InitRequest::new(Uuid::new_v4()))
            .await
            .unwrap()
            .project;
        let backend_path = root.path().join("backend.sock");
        let listener = UnixListener::bind(&backend_path).unwrap();
        fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
        let initial_root = Uuid::new_v4().to_string();
        let target = if same_root {
            initial_root.clone()
        } else {
            Uuid::new_v4().to_string()
        };
        let backend = tokio::spawn(fake_codex(
            listener,
            directory.clone(),
            directory.clone(),
            target.clone(),
            integration_namespace(manager.home()),
            false,
            manager.home().to_owned(),
        ));
        let proxy = Proxy::bind(manager.home(), &backend_path, &initial_root).unwrap();
        let go = root.path().join("go");
        let seen = root.path().join("seen");
        let environment = BTreeMap::from([
            (
                "AGENTCHAT_TEST_TRANSITION_PROXY".into(),
                proxy.path().as_os_str().into(),
            ),
            (
                "AGENTCHAT_TEST_TRANSITION_TARGET".into(),
                target.clone().into(),
            ),
            ("AGENTCHAT_TEST_TRANSITION_SUCCESS".into(), "1".into()),
            ("AGENTCHAT_TEST_TRANSITION_GO".into(), go.as_os_str().into()),
            (
                "AGENTCHAT_TEST_TRANSITION_SEEN".into(),
                seen.as_os_str().into(),
            ),
        ]);
        let arguments: Vec<OsString> = [
            "--ignored",
            "--exact",
            "operations::activation::codex_transition::tests::codex_transition_front_helper",
            "--nocapture",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let child = NativeChild::spawn(
            &std::env::current_exe().unwrap(),
            &directory,
            &arguments,
            &environment,
        )
        .await
        .unwrap();
        let proof = child.proof();
        let events = proxy.serve(proof.clone());
        let context = ContextRegistry::new(manager.clone())
            .issue(ContextSeed {
                host: HostKind::Codex,
                host_session_id: initial_root,
                host_version: "synthetic-transition".into(),
                os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                working_directory: directory.clone(),
            })
            .await
            .unwrap();
        let original = Binding::new(
            context,
            proof.identity().clone(),
            Endpoint::Codex {
                socket: backend_path,
                executable: proof.owner().executable(),
            },
        )
        .unwrap();
        NativeRegistry::new(manager.home())
            .publish(&original)
            .unwrap();
        let socket = ActivationSocket::bind(manager.home(), &original).unwrap();
        let ready = Registry::new(manager.home())
            .publish_ready(&original, socket.path().into(), None)
            .unwrap();
        let server = Server::bind(
            manager.clone(),
            project.clone(),
            std::sync::Arc::new(AdmittedHosts::new(manager.home().into())),
            std::sync::Arc::new(SystemClock),
            RuntimeOptions {
                idle: Duration::from_secs(10),
                io_timeout: Duration::from_secs(1),
                housekeeping: Duration::from_millis(20),
            },
        )
        .await
        .unwrap();
        let runtime = server.handle();
        let runtime_task = tokio::spawn(server.run());
        let operations = Operations::new(manager, std::sync::Arc::new(NoLaunch));
        let route = crate::operations::Route {
            project,
            directory,
            context: None,
            user: false,
        };
        let mut control = Control::new(socket, ready, Native::Codex { proxy: events });
        control
            .activate_initial(&operations, &route, &proof)
            .await
            .unwrap();
        Self {
            _root: root,
            operations,
            route,
            control,
            original,
            proof,
            child,
            runtime,
            runtime_task,
            backend,
            go,
            seen,
            target,
        }
    }
    fn registry(&self) -> Registry {
        Registry::new(self.operations.manager.home())
    }
    async fn tick(&mut self) {
        self.control
            .tick(&self.operations, &self.route, &self.proof)
            .await
            .unwrap();
    }
    async fn rejoin(&mut self) {
        let before = self.control.binding.clone();
        let mut client = self.operations.connect(&self.route).await.unwrap();
        let id = Uuid::new_v4();
        success(
            client
                .call(
                    id,
                    Call::Mutate {
                        request_id: id,
                        actor: agentchat_protocol::rpc::ActorReference::Native {
                            context_id: before.context.context_id,
                        },
                        command: Mutation::Leave,
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        self.tick().await;
        assert!(self.control.started.is_none());
        let request = crate::operations::activation::wire::Request {
            version: 1,
            id: Uuid::new_v4(),
            context: before.context.context_id,
            command: Mutation::Join { name: None },
        };
        self.control
            .activate(&self.operations, &self.route, &self.proof, &request)
            .await
            .unwrap();
        // A live RPC connection is bound to one exact context.
        drop(client);
        let mut client = self.operations.connect(&self.route).await.unwrap();
        let after = &self.control.binding;
        assert_ne!(after.context.context_id, before.context.context_id);
        assert_ne!(after.context.incarnation, before.context.incarnation);
        assert_ne!(after.identity.generation, before.identity.generation);
        assert_eq!(after.identity.owner, before.identity.owner);
        assert_eq!(
            client
                .native_binding(after.context.context_id)
                .await
                .unwrap()
                .participation,
            Participation::Joined
        );
        assert!(self.registry().was_started(after).unwrap());
    }
    async fn recover_descriptor(&mut self) {
        let before = self.control.binding.clone();
        let replacement = self
            .control
            .fresh(&self.operations, &self.proof, false)
            .await
            .unwrap();
        self.control.pending = Some(replacement);
        self.tick().await;
        assert_ne!(
            self.control.binding.context.context_id,
            before.context.context_id
        );
        assert_ne!(
            self.control.binding.context.incarnation,
            before.context.incarnation
        );
        assert_eq!(self.control.binding.identity, before.identity);
        assert!(self.control.attachment.is_some());
    }
    async fn native_switch(&self) {
        fs::write(
            self.operations
                .manager
                .home()
                .join("expected-switch-binding.json"),
            serde_json::to_vec(&self.control.binding).unwrap(),
        )
        .unwrap();
        fs::write(&self.go, b"switch only after activation").unwrap();
        wait_until(|| self.seen.exists()).await;
        // No Control tick or event drain: the proxy itself must fence the
        // current generation before the backend sees the complete request.
        let current = &self.control.binding;
        assert!(self.registry().root_changed(
            current.context.project_id,
            current.context.host,
            &current.context.host_session_id,
            &current.identity
        ));
    }
    async fn settle(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            self.tick().await;
            if self.control.binding.context.host_session_id == self.target
                && self.control.codex_candidate.is_none()
                && !self.registry().root_changed(
                    self.control.binding.context.project_id,
                    self.control.binding.context.host,
                    &self.control.binding.context.host_session_id,
                    &self.control.binding.identity,
                )
            {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    async fn finish(mut self) {
        fs::write(self.seen.with_extension("finish"), b"normal native exit").unwrap();
        assert!(self.child.wait().await.unwrap().success());
        self.runtime.shutdown();
        self.runtime_task.await.unwrap().unwrap();
        self.backend.abort();
    }
}

#[tokio::test]
async fn explicit_rejoin_fences_current_generation_before_native_switch() {
    let mut f = Fixture::new(false).await;
    f.rejoin().await;
    let Native::Codex { proxy } = &f.control.native else {
        unreachable!()
    };
    assert!(
        proxy.track(&f.original).is_err(),
        "retired descriptor was accepted"
    );
    let current = &f.control.binding;
    let mut fabricated = current.clone();
    fabricated.context.incarnation = Uuid::new_v4();
    assert!(proxy.track(&fabricated).is_err());
    fabricated = current.clone();
    fabricated.identity.generation = Uuid::new_v4();
    assert!(proxy.track(&fabricated).is_err());
    fabricated = current.clone();
    fabricated.context.project_id = Uuid::new_v4();
    assert!(proxy.track(&fabricated).is_err());
    fabricated = current.clone();
    fabricated.identity.owner = f.original.identity.generation.to_string();
    assert!(proxy.track(&fabricated).is_err());
    proxy.track(current).unwrap();
    let departed = current.clone();
    f.native_switch().await;
    f.settle().await;
    let Native::Codex { proxy } = &f.control.native else {
        unreachable!()
    };
    assert!(
        proxy.track(&departed).is_err(),
        "departed root was accepted"
    );
    // Physical readiness still belongs to the original launch, even after a
    // distinct-root activation and another explicit participation generation.
    f.rejoin().await;
    f.finish().await;
}

#[tokio::test]
async fn descriptor_recovery_after_rejoin_fences_current_generation_before_native_switch() {
    let mut f = Fixture::new(false).await;
    f.rejoin().await;
    let retired = f.control.binding.clone();
    f.recover_descriptor().await;
    let Native::Codex { proxy } = &f.control.native else {
        unreachable!()
    };
    assert!(proxy.track(&retired).is_err());
    f.native_switch().await;
    f.settle().await;
    f.finish().await;
}

#[tokio::test]
async fn unresolved_switch_refuses_same_root_descriptor_retarget() {
    let mut f = Fixture::new(false).await;
    f.native_switch().await;
    let replacement = f
        .control
        .fresh(&f.operations, &f.proof, false)
        .await
        .unwrap();
    assert!(
        f.control
            .publish(&f.operations, &replacement)
            .await
            .is_err()
    );
    let Native::Codex { proxy } = &f.control.native else {
        unreachable!()
    };
    assert!(proxy.track(&replacement).is_err());
    assert!(f.control.mark_delivery_started(&f.registry()).is_err());
    // Native response and subsequent non-lifecycle RPC still complete while
    // AgentChat refuses to lend an unresolved fence to another descriptor.
    f.finish().await;
}

#[tokio::test]
async fn acknowledged_same_root_rollback_allows_descriptor_recovery() {
    let mut f = Fixture::new(true).await;
    f.native_switch().await;
    f.settle().await;
    let before = f.control.binding.clone();
    assert!(!f.registry().root_changed(
        before.context.project_id,
        before.context.host,
        &before.context.host_session_id,
        &before.identity
    ));
    // The control ACK is asynchronous; wait for that exact ACK to release the
    // observer's retarget barrier, without treating Unknown as rollback.
    let replacement = f
        .control
        .fresh(&f.operations, &f.proof, false)
        .await
        .unwrap();
    f.control
        .publish(&f.operations, &replacement)
        .await
        .unwrap();
    let Native::Codex { proxy } = &f.control.native else {
        unreachable!()
    };
    wait_until(|| proxy.track(&replacement).is_ok()).await;
    f.control.pending = Some(replacement);
    f.tick().await;
    assert_eq!(f.control.binding.identity, before.identity);
    assert_ne!(
        f.control.binding.context.context_id,
        before.context.context_id
    );
    f.finish().await;
}
