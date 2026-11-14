use super::*;
use agentchat_core::ports::HostGate;

struct ResumeDuringVerify {
    inner: AdmittedHosts,
    first: std::sync::atomic::AtomicBool,
    unavailable: std::sync::Arc<std::sync::atomic::AtomicBool>,
    go: std::path::PathBuf,
    seen: std::path::PathBuf,
}

#[async_trait::async_trait]
impl HostGate for ResumeDuringVerify {
    fn input_capabilities(
        &self,
        context: &Context,
    ) -> Result<agentchat_core::delivery::InputCapabilities> {
        self.inner.input_capabilities(context)
    }

    async fn verify(&self, context: &Context) -> Result<agentchat_core::ports::HostState> {
        if self.unavailable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::new(
                ErrorCode::RuntimeUnavailable,
                "Owned test runtime unavailable.",
            ));
        }
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            // Control already drained before entering this actual Core RPC.
            // Native forwarding establishes the fence before the real gate reads it.
            fs::write(&self.go, b"resume during Core verification").unwrap();
            wait_until(|| self.seen.exists()).await;
        }
        self.inner.verify(context).await
    }

    async fn launch_identity(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::model::LaunchIdentity>> {
        self.inner.launch_identity(context).await
    }

    async fn launch_delivery_identity(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::model::LaunchIdentity>> {
        self.inner.launch_delivery_identity(context).await
    }

    async fn launch_name(&self, context: &Context) -> Result<Option<String>> {
        self.inner.launch_name(context).await
    }

    fn launch_owner(
        &self,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> agentchat_core::model::LaunchOwnerState {
        self.inner.launch_owner(identity)
    }

    fn launch_root_changed(
        &self,
        project: Uuid,
        host: HostKind,
        root: &str,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> bool {
        self.inner
            .launch_root_changed(project, host, root, identity)
    }

    fn launch_root_departed(
        &self,
        project: Uuid,
        host: HostKind,
        root: &str,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> bool {
        self.inner
            .launch_root_departed(project, host, root, identity)
    }
}

#[derive(Clone, Copy)]
enum Cut {
    SameRoot,
    DuringRpc,
    Rejected,
    Unavailable,
    AckLost,
    WrongReply,
    DistinctRoot,
    Left,
}

#[tokio::test]
async fn initial_same_root_resume_releases_its_exact_fence_before_join() {
    run_initial_cut(Cut::SameRoot).await;
}

#[tokio::test]
async fn initial_resume_during_core_verification_retries_the_same_intent() {
    run_initial_cut(Cut::DuringRpc).await;
}

#[tokio::test]
async fn initial_rejected_resume_releases_only_its_exact_fence() {
    run_initial_cut(Cut::Rejected).await;
}

#[tokio::test]
async fn initial_unknown_keeps_native_forwarding_without_join() {
    run_initial_cut(Cut::WrongReply).await;
}

#[tokio::test]
async fn initial_distinct_root_never_grants_the_old_initial_context() {
    run_initial_cut(Cut::DistinctRoot).await;
}

#[tokio::test]
async fn initial_receipt_replay_after_leave_does_not_restart_participation() {
    run_initial_cut(Cut::Left).await;
}

#[tokio::test]
async fn initial_transient_timeout_retains_owned_native_and_retries_original_intent() {
    run_initial_cut(Cut::Unavailable).await;
}

#[tokio::test]
async fn initial_expired_observer_ack_keeps_native_and_its_unknown_fence() {
    run_initial_cut(Cut::AckLost).await;
}

async fn run_initial_cut(cut: Cut) {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let workspace = root.path().join("project");
    fs::create_dir(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let manager = ProjectManager::new(root.path().join("state")).unwrap();
    let project = manager
        .initialize(&workspace, InitRequest::new(Uuid::new_v4()))
        .await
        .unwrap()
        .project;
    let backend_path = root.path().join("backend.sock");
    let backend = UnixListener::bind(&backend_path).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
    let initial_root = Uuid::new_v4().to_string();
    let reply_root = if matches!(cut, Cut::WrongReply | Cut::DistinctRoot) {
        Uuid::new_v4().to_string()
    } else {
        initial_root.clone()
    };
    let request_root = if matches!(cut, Cut::DistinctRoot) {
        reply_root.clone()
    } else {
        initial_root.clone()
    };
    let server = tokio::spawn(fake_codex(
        backend,
        workspace.clone(),
        workspace.clone(),
        reply_root.clone(),
        integration_namespace(manager.home()),
        false,
        manager.home().to_owned(),
    ));
    let proxy = Proxy::bind(manager.home(), &backend_path, &initial_root).unwrap();
    let seen = root.path().join("native-reply");
    let go = root.path().join("native-go");
    let mut environment = BTreeMap::from([
        (
            OsString::from("AGENTCHAT_TEST_TRANSITION_PROXY"),
            proxy.path().as_os_str().into(),
        ),
        (
            "AGENTCHAT_TEST_TRANSITION_TARGET".into(),
            request_root.into(),
        ),
        (
            "AGENTCHAT_TEST_TRANSITION_REPLY_ROOT".into(),
            reply_root.into(),
        ),
        ("AGENTCHAT_TEST_TRANSITION_SUCCESS".into(), "1".into()),
        ("AGENTCHAT_TEST_TRANSITION_GO".into(), go.as_os_str().into()),
        (
            "AGENTCHAT_TEST_TRANSITION_SEEN".into(),
            seen.as_os_str().into(),
        ),
    ]);
    if matches!(cut, Cut::Rejected) {
        environment.remove(&OsString::from("AGENTCHAT_TEST_TRANSITION_SUCCESS"));
        fs::write(
            manager.home().join("resume-rejected"),
            b"definite native rejection",
        )
        .unwrap();
    }
    if matches!(cut, Cut::Unavailable | Cut::AckLost) {
        // These new cases span the unchanged application/observer retry budgets.
        environment.insert(
            "AGENTCHAT_TEST_TRANSITION_FINISH_SECONDS".into(),
            "45".into(),
        );
    }
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from(
            "operations::activation::codex_transition::tests::codex_transition_front_helper",
        ),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(
        &std::env::current_exe().unwrap(),
        &workspace,
        &arguments,
        &environment,
    )
    .await
    .unwrap();
    let proof = child.proof();
    let proxy = proxy.serve(proof.clone());
    let context = ContextRegistry::new(manager.clone())
        .issue(ContextSeed {
            host: HostKind::Codex,
            host_session_id: initial_root,
            host_version: "synthetic-transition".into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: workspace.clone(),
        })
        .await
        .unwrap();
    let binding = Binding::new(
        context,
        proof.identity().clone(),
        Endpoint::Codex {
            socket: backend_path,
            executable: proof.owner().executable(),
        },
    )
    .unwrap();
    NativeRegistry::new(manager.home())
        .publish(&binding)
        .unwrap();
    let registry = Registry::new(manager.home());
    let socket = ActivationSocket::bind(manager.home(), &binding).unwrap();
    let ready = registry
        .publish_ready(&binding, socket.path().into(), None)
        .unwrap();
    let unavailable = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(matches!(
        cut,
        Cut::Unavailable
    )));
    let gate: std::sync::Arc<dyn agentchat_core::ports::HostGate> =
        if matches!(cut, Cut::DuringRpc | Cut::Unavailable) {
            std::sync::Arc::new(ResumeDuringVerify {
                inner: AdmittedHosts::new(manager.home().into()),
                first: std::sync::atomic::AtomicBool::new(matches!(cut, Cut::DuringRpc)),
                unavailable: unavailable.clone(),
                go: go.clone(),
                seen: seen.clone(),
            })
        } else {
            std::sync::Arc::new(AdmittedHosts::new(manager.home().into()))
        };
    let runtime = Server::bind(
        manager.clone(),
        project.clone(),
        gate,
        std::sync::Arc::new(SystemClock),
        RuntimeOptions {
            idle: Duration::from_secs(10),
            io_timeout: Duration::from_secs(1),
            housekeeping: Duration::from_millis(20),
        },
    )
    .await
    .unwrap();
    let handle = runtime.handle();
    let runtime_task = tokio::spawn(runtime.run());
    let operations = Operations::new(manager.clone(), std::sync::Arc::new(NoLaunch));
    let route = super::super::super::super::Route {
        project,
        directory: workspace,
        context: None,
        user: false,
    };
    let mut control = Control::new(socket, ready, Native::Codex { proxy });
    let started_path = manager
        .home()
        .join("hosts/launches/started")
        .join(format!("{}.json", binding.identity.generation));
    let original_started = if matches!(cut, Cut::Left) {
        assert!(
            control
                .activate_initial(&operations, &route, &proof)
                .await
                .unwrap()
        );
        let mut client = operations.connect(&route).await.unwrap();
        let request_id = Uuid::new_v4();
        agentchat_protocol::rpc::success(
            client
                .call(
                    request_id,
                    agentchat_protocol::rpc::Call::Mutate {
                        request_id,
                        actor: agentchat_protocol::rpc::ActorReference::Native {
                            context_id: binding.context.context_id,
                        },
                        command: agentchat_core::application::Mutation::Leave,
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        control.tick(&operations, &route, &proof).await.unwrap();
        Some(fs::read(&started_path).unwrap())
    } else {
        None
    };
    let observer = if matches!(cut, Cut::AckLost) {
        Some(operations.connect(&route).await.unwrap())
    } else {
        None
    };
    // Fixture-only context-free activity keeps the owned test runtime available
    // while the real 35-second observer ACK deadline is deliberately left alone.
    let startup_activity = match observer {
        Some(client) => Some(client.observe().await.unwrap()),
        None => None,
    };
    control.track_codex_binding(&binding).unwrap();
    let native_cut_started = tokio::time::Instant::now();
    // Reproduce the startup cut: immutable readiness is published and tracked,
    // but no Control tick has acknowledged the native same-root resume yet.
    if !matches!(cut, Cut::DuringRpc) {
        fs::write(go, b"physical readiness tracked before initial Join").unwrap();
        wait_until(|| seen.exists()).await;
        assert!(registry.root_changed(
            binding.context.project_id,
            binding.context.host,
            &binding.context.host_session_id,
            &binding.identity,
        ));
    }
    if matches!(cut, Cut::AckLost) {
        let Native::Codex { proxy } = &control.native else {
            unreachable!()
        };
        tokio::time::timeout(Duration::from_secs(40), async {
            while proxy.track(&binding).is_ok() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        assert!(native_cut_started.elapsed() >= Duration::from_secs(35));
        assert_eq!(
            proof.state(),
            agentchat_core::model::LaunchOwnerState::Current
        );
    }
    let original_request = control.initial_request;
    let started_at = tokio::time::Instant::now();
    let result = control.activate_initial(&operations, &route, &proof).await;
    if matches!(cut, Cut::Unavailable) {
        assert!(started_at.elapsed() >= Duration::from_secs(30));
        assert!(matches!(result, Ok(false)));
        assert!(control.initial_waiting);
        assert!(control.started.is_none() && control.attachment.is_none());
        assert_eq!(
            proof.state(),
            agentchat_core::model::LaunchOwnerState::Current
        );
        assert_eq!(
            manager
                .open(&route.project)
                .await
                .unwrap()
                .snapshot()
                .await
                .unwrap()
                .statistics()
                .await
                .unwrap()
                .agents,
            0
        );
        let competing = crate::operations::activation::wire::Request {
            version: 1,
            id: Uuid::new_v4(),
            context: binding.context.context_id,
            command: agentchat_core::application::Mutation::Join { name: None },
        };
        let error = control
            .activate(&operations, &route, &proof, &competing)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Busy);
        assert!(control.pending.is_none() && control.pending_request.is_none());
        assert_eq!(control.initial_request, original_request);
        unavailable.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    // A deferred initial intent is kept by the same physical-ready Control.
    // Further ticks cannot turn Unknown, distinct success, or Leave into Join.
    control.tick(&operations, &route, &proof).await.unwrap();
    assert_eq!(
        proof.state(),
        agentchat_core::model::LaunchOwnerState::Current
    );
    if matches!(cut, Cut::WrongReply | Cut::AckLost) {
        let Native::Codex { proxy } = &control.native else {
            unreachable!()
        };
        assert!(
            proxy.track(&binding).is_err(),
            "Unknown observer was re-enabled"
        );
    }
    let mut client = operations.connect(&route).await.unwrap();
    let participation = client
        .native_binding(binding.context.context_id)
        .await
        .unwrap()
        .participation;
    drop(client);
    let statistics = manager
        .open(&route.project)
        .await
        .unwrap()
        .snapshot()
        .await
        .unwrap()
        .statistics()
        .await
        .unwrap();
    drop(startup_activity);
    fs::write(seen.with_extension("finish"), b"normal native exit").unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    handle.shutdown();
    runtime_task.await.unwrap().unwrap();
    server.abort();
    assert!(exit.success());
    assert!(
        result.is_ok(),
        "initial resume fenced its own Join: {result:?}"
    );
    assert!(seen.exists(), "the intended native RPC cut was not reached");
    assert_eq!(control.initial_request, original_request);
    assert_eq!(control.binding, binding);
    let active = matches!(
        cut,
        Cut::SameRoot | Cut::DuringRpc | Cut::Rejected | Cut::Unavailable
    );
    assert_eq!(result.unwrap(), active && !matches!(cut, Cut::Unavailable));
    assert_eq!(control.started.is_some(), active);
    assert_eq!(control.attachment.is_some(), active);
    assert_eq!(
        statistics.agents,
        u64::from(active || matches!(cut, Cut::Left))
    );
    assert_eq!(statistics.messages, 0);
    assert!(
        serde_json::to_value(&statistics)
            .unwrap()
            .get("acknowledged_deliveries")
            .is_none()
    );
    assert_eq!(
        registry.was_started(&binding).unwrap(),
        active || matches!(cut, Cut::Left)
    );
    assert_eq!(
        participation,
        match cut {
            Cut::SameRoot | Cut::DuringRpc | Cut::Rejected | Cut::Unavailable =>
                Participation::Joined,
            Cut::Left => Participation::Left,
            Cut::WrongReply | Cut::DistinctRoot | Cut::AckLost => Participation::Unjoined,
        }
    );
    if let Some(bytes) = original_started {
        assert_eq!(fs::read(started_path).unwrap(), bytes);
        assert!(!control.initial_waiting);
    }
    if matches!(cut, Cut::WrongReply | Cut::DistinctRoot | Cut::AckLost) {
        assert!(control.initial_waiting);
        assert!(registry.root_changed(
            binding.context.project_id,
            binding.context.host,
            &binding.context.host_session_id,
            &binding.identity
        ));
    }
}
