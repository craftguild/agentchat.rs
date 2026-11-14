//! A committed initial Join survives loss of its first MCP incarnation.
use super::*;
use agentchat_core::application::{ManagedActivationCause, ManagedActivationResult};
use agentchat_protocol::rpc::{Call, success};

#[derive(Clone, Copy)]
enum Cut {
    Restore,
    LeftBeforeReplay,
    LeftBeforeReplacement,
    UnknownBeforeReplacement,
    UnavailableBeforeReplacement,
    MarkerRetry,
    MarkerThenLeft,
    OriginalMarkerCommitted,
    PendingMcpDisconnect,
}

#[tokio::test]
async fn initial_committed_disconnect_restores_only_a_fresh_same_generation_attachment() {
    run_cut(Cut::Restore).await;
}

#[tokio::test]
async fn initial_committed_receipt_after_leave_never_restores_or_rejoins() {
    run_cut(Cut::LeftBeforeReplay).await;
}

#[tokio::test]
async fn initial_committed_replacement_observes_a_later_leave() {
    run_cut(Cut::LeftBeforeReplacement).await;
}

#[tokio::test]
async fn initial_committed_replacement_cannot_move_an_unknown_fence() {
    run_cut(Cut::UnknownBeforeReplacement).await;
}

#[tokio::test]
async fn initial_committed_ambiguous_publication_keeps_one_pending_descriptor() {
    run_cut(Cut::UnavailableBeforeReplacement).await;
}

#[tokio::test]
async fn initial_recovered_marker_retry_keeps_the_current_descriptor_and_receipt() {
    run_cut(Cut::MarkerRetry).await;
}

#[tokio::test]
async fn initial_recovered_marker_retry_cannot_revive_a_later_leave() {
    run_cut(Cut::MarkerThenLeft).await;
}

#[tokio::test]
async fn initial_recovery_preserves_an_already_committed_original_start_marker() {
    run_cut(Cut::OriginalMarkerCommitted).await;
}

#[tokio::test]
async fn initial_pending_incarnation_disconnect_requires_another_verified_descriptor() {
    run_cut(Cut::PendingMcpDisconnect).await;
}

async fn run_cut(cut: Cut) {
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
    let native_root = Uuid::new_v4().to_string();
    let backend = tokio::spawn(fake_codex(
        listener,
        directory.clone(),
        directory.clone(),
        native_root.clone(),
        integration_namespace(manager.home()),
        false,
        manager.home().to_owned(),
    ));
    let proxy = Proxy::bind(manager.home(), &backend_path, &native_root).unwrap();
    let executable = fs::canonicalize("/bin/sleep").unwrap();
    let mut child = NativeChild::spawn(&executable, &directory, &["120".into()], &BTreeMap::new())
        .await
        .unwrap();
    let proof = child.proof();
    let proxy = proxy.serve(proof.clone());
    let context = ContextRegistry::new(manager.clone())
        .issue(ContextSeed {
            host: HostKind::Codex,
            host_session_id: native_root,
            host_version: "synthetic-transition".into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: directory.clone(),
        })
        .await
        .unwrap();
    let binding = Binding::new(
        context,
        proof.identity().clone(),
        Endpoint::Codex {
            socket: backend_path,
            executable,
        },
    )
    .unwrap();
    NativeRegistry::new(manager.home())
        .publish(&binding)
        .unwrap();
    let socket = ActivationSocket::bind(manager.home(), &binding).unwrap();
    let registry = Registry::new(manager.home());
    let ready = registry
        .publish_ready(&binding, socket.path().into(), None)
        .unwrap();
    let server = Server::bind(
        manager.clone(),
        project.clone(),
        std::sync::Arc::new(AdmittedHosts::new(manager.home().into())),
        std::sync::Arc::new(SystemClock),
        RuntimeOptions {
            idle: Duration::from_secs(10),
            io_timeout: Duration::from_secs(1),
            housekeeping: Duration::from_millis(5),
        },
    )
    .await
    .unwrap();
    let runtime = server.handle();
    let runtime_task = tokio::spawn(server.run());
    let operations = Operations::new(manager.clone(), std::sync::Arc::new(NoLaunch));
    let route = super::super::super::super::Route {
        project,
        directory,
        context: None,
        user: false,
    };
    let mut control = Control::new(socket, ready, Native::Codex { proxy });
    let original_request = control.initial_request;
    let mut mcp = operations.connect(&route).await.unwrap();
    // The durable operation committed, but its caller has not finished startup.
    // A native MCP connects and then actually drops its last owned RPC socket.
    let value = success(
        mcp.call(
            original_request,
            Call::ActivateManaged {
                request_id: original_request,
                context_id: binding.context.context_id,
                cause: ManagedActivationCause::Initial,
                name: None,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let activated: ManagedActivationResult = serde_json::from_value(value).unwrap();
    success(
        mcp.call(
            Uuid::new_v4(),
            Call::Attach {
                context_id: binding.context.context_id,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let original_marker = if matches!(cut, Cut::OriginalMarkerCommitted) {
        registry
            .mark_started(&binding, activated.join_boundary)
            .unwrap();
        Some(
            fs::read(
                manager
                    .home()
                    .join("hosts/launches/started")
                    .join(format!("{}.json", binding.identity.generation)),
            )
            .unwrap(),
        )
    } else {
        None
    };
    drop(mcp);
    let mut observer = operations.connect(&route).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let state = observer
                .native_binding(binding.context.context_id)
                .await
                .unwrap();
            if state.agent.is_some_and(|agent| !agent.connected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let stale = observer
        .call(
            Uuid::new_v4(),
            Call::Attach {
                context_id: binding.context.context_id,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::IdentityConflict);
    assert_eq!(
        stale.message,
        "Reconnect requires a fresh connection incarnation."
    );
    let before_events = manager
        .open(&route.project)
        .await
        .unwrap()
        .snapshot()
        .await
        .unwrap()
        .statistics()
        .await
        .unwrap()
        .events;
    if matches!(cut, Cut::LeftBeforeReplay) {
        leave(&mut observer, &binding).await;
    }
    let result = control.activate_initial(&operations, &route, &proof).await;
    let prepared_context = control
        .pending
        .as_ref()
        .map(|pending| pending.context.context_id);
    let mut marker_error = None;
    if result.is_ok() {
        match cut {
            Cut::LeftBeforeReplacement => leave(&mut observer, &binding).await,
            Cut::UnknownBeforeReplacement => {
                // The committed receipt does not permit transferring an exact
                // unresolved observation fence to its prepared replacement.
                registry.record_managed_transition(&binding, 7, 11).unwrap();
                control.codex_candidate = Some((7, 11, true));
                control.attachment = None;
                control.started = None;
            }
            Cut::UnavailableBeforeReplacement => {
                // Fail the real metadata observation after the pending route
                // is durably published. Repeated attempts cannot mint a route.
                backend.abort();
                tokio::task::yield_now().await;
            }
            Cut::MarkerRetry | Cut::MarkerThenLeft => {
                control
                    .restore_pending_attachment(&operations)
                    .await
                    .unwrap();
                let path = manager.home().join("hosts/launches/started");
                fs::create_dir(&path).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o500)).unwrap();
                marker_error = control.mark_delivery_started(&registry).err();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                if matches!(cut, Cut::MarkerThenLeft) {
                    let mut current = operations.connect(&route).await.unwrap();
                    leave(&mut current, &control.binding).await;
                }
            }
            Cut::PendingMcpDisconnect => {
                let pending = control.pending.as_ref().unwrap().clone();
                control.publish(&operations, &pending).await.unwrap();
                let mut mcp = operations.connect(&route).await.unwrap();
                success(
                    mcp.call(
                        Uuid::new_v4(),
                        Call::Attach {
                            context_id: pending.context.context_id,
                        },
                    )
                    .await
                    .unwrap(),
                )
                .unwrap();
                drop(mcp);
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let state = observer
                            .native_binding(pending.context.context_id)
                            .await
                            .unwrap();
                        if state.agent.is_some_and(|agent| !agent.connected) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
            }
            _ => {}
        }
    }
    let mut recovery_error = None;
    if result.is_ok() {
        let ticks = if matches!(cut, Cut::UnavailableBeforeReplacement) {
            1
        } else {
            4
        };
        for _ in 0..ticks {
            if let Err(error) = control.tick(&operations, &route, &proof).await {
                recovery_error = Some(error);
                break;
            }
        }
    }
    let current = control.binding.clone();
    let attached = control.attachment.is_some();
    let started = control.started.clone();
    let owner_current = proof.state() == agentchat_core::model::LaunchOwnerState::Current;
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
    let retained_request = control.initial_request;
    let pending_context = control
        .pending
        .as_ref()
        .map(|pending| pending.context.context_id);
    let fenced = registry.root_changed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    );
    let marker_exists = registry.was_started(&current).unwrap();
    let marker_bytes = original_marker.as_ref().map(|_| {
        fs::read(
            manager
                .home()
                .join("hosts/launches/started")
                .join(format!("{}.json", binding.identity.generation)),
        )
        .unwrap()
    });
    child.terminate().await.unwrap();
    runtime.shutdown();
    runtime_task.await.unwrap().unwrap();
    backend.abort();
    assert!(
        result.is_ok(),
        "post-commit initial attachment terminated startup: {result:?}"
    );
    assert!(recovery_error.is_none(), "{recovery_error:?}");
    assert!(owner_current);
    assert_eq!(retained_request, original_request);
    assert_eq!(current.identity, binding.identity);
    assert_eq!(statistics.agents, 1);
    assert_eq!(statistics.messages, 0);
    assert!(
        serde_json::to_value(&statistics)
            .unwrap()
            .get("acknowledged_deliveries")
            .is_none()
    );
    assert_eq!(statistics.read_receipts, 0);
    match cut {
        Cut::Restore
        | Cut::MarkerRetry
        | Cut::OriginalMarkerCommitted
        | Cut::PendingMcpDisconnect => {
            assert!(attached);
            assert_ne!(current.context.context_id, binding.context.context_id);
            assert_ne!(current.context.incarnation, binding.context.incarnation);
            let started = started.unwrap();
            assert_eq!(started.agent_id, activated.agent_id);
            assert_eq!(started.boundary, activated.join_boundary);
            assert_eq!(statistics.events, before_events);
            assert!(marker_exists);
            if matches!(cut, Cut::MarkerRetry) {
                assert!(marker_error.is_some());
                assert_eq!(Some(current.context.context_id), prepared_context);
            }
            if matches!(cut, Cut::OriginalMarkerCommitted) {
                assert_eq!(marker_bytes, original_marker);
            }
            if matches!(cut, Cut::PendingMcpDisconnect) {
                assert_ne!(Some(current.context.context_id), prepared_context);
            }
        }
        Cut::LeftBeforeReplay | Cut::LeftBeforeReplacement | Cut::MarkerThenLeft => {
            assert!(!attached && started.is_none());
            assert!(pending_context.is_none());
            assert_eq!(statistics.events, before_events + 1);
            assert!(!marker_exists);
            if matches!(cut, Cut::MarkerThenLeft) {
                assert!(marker_error.is_some());
            }
        }
        Cut::UnknownBeforeReplacement => {
            assert!(!attached && started.is_none());
            assert!(fenced);
            assert_eq!(current.context, binding.context);
            assert!(pending_context.is_some());
            assert_eq!(statistics.events, before_events);
        }
        Cut::UnavailableBeforeReplacement => {
            assert!(!attached);
            assert_eq!(pending_context, prepared_context);
            assert!(pending_context.is_some());
            assert_eq!(current.context, binding.context);
            assert_eq!(statistics.events, before_events);
        }
    }
}

async fn leave(client: &mut agentchat_protocol::rpc::Client, binding: &Binding) {
    let request_id = Uuid::new_v4();
    success(
        client
            .call(
                request_id,
                Call::Mutate {
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
}
