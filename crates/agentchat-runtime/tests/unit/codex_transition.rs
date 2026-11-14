use super::*;
use crate::{
    context::{ContextRegistry, ContextSeed},
    daemon::{RuntimeOptions, Server},
    operations::Launcher,
    project::{InitRequest, Project, ProjectManager},
};
use agentchat_core::{
    Result,
    model::{Context, HostKind},
    ports::Storage,
    read::Participation,
    time::SystemClock,
};
use agentchat_hosts::{
    AdmittedHosts,
    codex::{NativeRegistry, Proxy, integration_namespace},
    launch::{ActivationSocket, Endpoint, LaunchOwners, NativeChild},
};
use futures_util::{SinkExt, StreamExt};
use std::{collections::BTreeMap, ffi::OsString, fs, os::unix::fs::PermissionsExt, time::Duration};
use tokio::net::{UnixListener, UnixStream};
use tokio_tungstenite::{accept_async, client_async, tungstenite::Message};

#[tokio::test]
#[ignore = "owned frontend helper invoked by control_serializes_proxy_fence_and_rollback"]
async fn codex_transition_front_helper() {
    let path = std::env::var_os("AGENTCHAT_TEST_TRANSITION_PROXY").unwrap();
    let target = std::env::var("AGENTCHAT_TEST_TRANSITION_TARGET").unwrap();
    let stream = UnixStream::connect(path).await.unwrap();
    let (mut socket, _) = client_async("ws://localhost/", stream).await.unwrap();
    if let Some(go) = std::env::var_os("AGENTCHAT_TEST_TRANSITION_GO") {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !std::path::Path::new(&go).exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
    socket
        .send(Message::Text(
            serde_json::json!({"id":17,"method":"thread/resume","params":{"threadId":target}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let response = socket.next().await;
    if std::env::var_os("AGENTCHAT_TEST_TRANSITION_UNKNOWN").is_some()
        && std::env::var_os("AGENTCHAT_TEST_TRANSITION_SUCCESS").is_none()
    {
        assert!(response.is_none() || response.is_some_and(|value| value.is_err()));
        return;
    }
    let response = response.unwrap().unwrap().into_text().unwrap();
    let response = serde_json::from_str::<serde_json::Value>(&response).unwrap();
    if std::env::var_os("AGENTCHAT_TEST_TRANSITION_SUCCESS").is_some() {
        let expected = std::env::var("AGENTCHAT_TEST_TRANSITION_REPLY_ROOT")
            .unwrap_or_else(|_| target.clone());
        assert_eq!(response["result"]["thread"]["id"], expected);
    } else {
        assert_eq!(response["error"]["code"], -32600);
    }
    if let Some(path) = std::env::var_os("AGENTCHAT_TEST_TRANSITION_SEEN") {
        let path = std::path::PathBuf::from(path);
        fs::write(&path, b"native reply received").unwrap();
        let finish_seconds = std::env::var("AGENTCHAT_TEST_TRANSITION_FINISH_SECONDS")
            .map(|value| value.parse::<u64>().unwrap())
            .unwrap_or(10);
        tokio::time::timeout(Duration::from_secs(finish_seconds), async {
            while !path.with_extension("finish").exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        socket.send(Message::Text(serde_json::json!({"id":18,"method":"thread/unsubscribe","params":{"threadId":target}}).to_string().into())).await.unwrap();
        let reply = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&reply).unwrap()["id"],
            18
        );
    }
}

struct NoLaunch;
#[async_trait::async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &std::path::Path) -> Result<()> {
        panic!("the focused test starts its runtime explicitly")
    }
}

#[tokio::test]
async fn control_serializes_proxy_fence_and_rollback() {
    let home = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let backend_path = home.path().join("backend.sock");
    let backend = UnixListener::bind(&backend_path).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
    let root = Uuid::new_v4().to_string();
    let target = Uuid::new_v4().to_string();
    let proxy = Proxy::bind(home.path(), &backend_path, &root).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert(
        OsString::from("AGENTCHAT_TEST_TRANSITION_PROXY"),
        proxy.path().as_os_str().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_TRANSITION_TARGET"),
        target.into(),
    );
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from(
            "operations::activation::codex_transition::tests::codex_transition_front_helper",
        ),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(&executable, home.path(), &arguments, &environment)
        .await
        .unwrap();
    let proof = child.proof();
    let events = proxy.serve(proof.clone());
    let binding = Binding::new(
        Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            host: HostKind::Codex,
            host_session_id: root,
            host_version: "synthetic-transition".into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: home.path().canonicalize().unwrap(),
        },
        proof.identity().clone(),
        Endpoint::Codex {
            socket: backend_path,
            executable: proof.owner().executable(),
        },
    )
    .unwrap();
    assert!(LaunchOwners::is_supervisor(&binding.identity));
    let registry = Registry::new(home.path());
    registry.publish(&binding).unwrap();
    let socket = ActivationSocket::bind(home.path(), &binding).unwrap();
    let ready = registry
        .publish_ready(&binding, socket.path().into(), None)
        .unwrap();
    let mut control = Control::new(socket, ready, Native::Codex { proxy: events });
    let server = tokio::spawn(async move {
        let (stream, _) = backend.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let request = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&request).unwrap()["method"],
            "thread/resume"
        );
        socket
            .send(Message::Text(
                serde_json::json!({"id":17,"error":{"code":-32600,"message":"cancelled"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });

    wait_until(|| {
        control.drain_proxy_events(&registry).unwrap();
        control.codex_candidate.is_some()
    })
    .await;
    assert!(registry.root_changed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    wait_until(|| {
        control.drain_proxy_events(&registry).unwrap();
        control.codex_candidate.is_none()
    })
    .await;
    assert!(!registry.root_changed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    assert!(child.wait().await.unwrap().success());
    server.await.unwrap();
}

#[tokio::test]
async fn unresolved_transport_retains_the_exact_delivery_fence() {
    let home = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let backend_path = home.path().join("backend.sock");
    let backend = UnixListener::bind(&backend_path).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
    let root = Uuid::new_v4().to_string();
    let proxy = Proxy::bind(home.path(), &backend_path, &root).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert(
        OsString::from("AGENTCHAT_TEST_TRANSITION_PROXY"),
        proxy.path().as_os_str().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_TRANSITION_TARGET"),
        Uuid::new_v4().to_string().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_TRANSITION_UNKNOWN"),
        "1".into(),
    );
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from(
            "operations::activation::codex_transition::tests::codex_transition_front_helper",
        ),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(&executable, home.path(), &arguments, &environment)
        .await
        .unwrap();
    let proof = child.proof();
    let events = proxy.serve(proof.clone());
    let binding = Binding::new(
        Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            host: HostKind::Codex,
            host_session_id: root,
            host_version: "synthetic-transition".into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: home.path().canonicalize().unwrap(),
        },
        proof.identity().clone(),
        Endpoint::Codex {
            socket: backend_path,
            executable: proof.owner().executable(),
        },
    )
    .unwrap();
    let registry = Registry::new(home.path());
    registry.publish(&binding).unwrap();
    let socket = ActivationSocket::bind(home.path(), &binding).unwrap();
    let ready = registry
        .publish_ready(&binding, socket.path().into(), None)
        .unwrap();
    let mut control = Control::new(socket, ready, Native::Codex { proxy: events });
    let server = tokio::spawn(async move {
        let (stream, _) = backend.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        assert!(socket.next().await.unwrap().unwrap().is_text());
    });
    wait_until(|| {
        control.drain_proxy_events(&registry).unwrap();
        control.codex_candidate.is_some_and(|value| value.2)
    })
    .await;
    assert!(registry.root_changed(
        binding.context.project_id,
        binding.context.host,
        &binding.context.host_session_id,
        &binding.identity,
    ));
    assert!(child.wait().await.unwrap().success());
    server.await.unwrap();
}

#[tokio::test]
async fn distinct_success_auto_activates_after_native_reply_delivery() {
    run_transition(false, false).await;
}

#[tokio::test]
async fn cross_project_success_retires_old_root_without_joining_target() {
    run_transition(true, false).await;
}

#[tokio::test]
async fn failed_target_mcp_preserves_native_transport_without_joining_target() {
    run_transition(false, true).await;
}

async fn run_transition(cross_project: bool, failed_mcp: bool) {
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
    let target_directory = if cross_project {
        let directory = root.path().join("other-project");
        fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        manager
            .initialize(&directory, InitRequest::new(Uuid::new_v4()))
            .await
            .unwrap();
        directory
    } else {
        workspace.clone()
    };
    let backend_path = root.path().join("backend.sock");
    let backend = UnixListener::bind(&backend_path).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
    let initial_root = Uuid::new_v4().to_string();
    let target_root = Uuid::new_v4().to_string();
    let server = tokio::spawn(fake_codex(
        backend,
        workspace.clone(),
        target_directory,
        target_root.clone(),
        integration_namespace(manager.home()),
        failed_mcp,
        manager.home().to_owned(),
    ));
    let proxy = Proxy::bind(manager.home(), &backend_path, &initial_root).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert(
        OsString::from("AGENTCHAT_TEST_TRANSITION_PROXY"),
        proxy.path().as_os_str().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_TRANSITION_TARGET"),
        target_root.clone().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_TRANSITION_SUCCESS"),
        "1".into(),
    );
    if failed_mcp {
        environment.insert("AGENTCHAT_TEST_TRANSITION_UNKNOWN".into(), "1".into());
    }
    let seen = root.path().join("native-reply");
    let go = root.path().join("native-go");
    environment.insert("AGENTCHAT_TEST_TRANSITION_GO".into(), go.as_os_str().into());
    environment.insert(
        "AGENTCHAT_TEST_TRANSITION_SEEN".into(),
        seen.as_os_str().into(),
    );
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from(
            "operations::activation::codex_transition::tests::codex_transition_front_helper",
        ),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(&executable, &workspace, &arguments, &environment)
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
    let runtime = Server::bind(
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
    let handle = runtime.handle();
    let runtime_task = tokio::spawn(runtime.run());
    let operations = Operations::new(manager.clone(), std::sync::Arc::new(NoLaunch));
    let route = super::super::super::Route {
        project,
        directory: workspace,
        context: None,
        user: false,
    };
    let mut control = Control::new(socket, ready, Native::Codex { proxy });
    control
        .activate_initial(&operations, &route, &proof)
        .await
        .unwrap();
    let initial = control.binding.context.context_id;
    fs::write(go, b"ready binding tracked").unwrap();
    // No application tick has consumed Candidate/Succeeded. The ready binding
    // must fence server entry and native response forwarding independently.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !seen.exists() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(registry.root_changed(
        control.binding.context.project_id,
        control.binding.context.host,
        &control.binding.context.host_session_id,
        &control.binding.identity
    ));
    if failed_mcp {
        let original = control.binding.clone();
        struct Faults(tokio::sync::mpsc::UnboundedSender<serde_json::Value>);
        impl crate::launch::LaunchEvents for Faults {
            fn connection_lost(&self, error: &Error) -> Result<()> {
                let _ = self.0.send(serde_json::json!({"code":error.code,"details":error.details,"retryable":error.retryable}));
                Ok(())
            }
        }
        let (sent, mut faults) = tokio::sync::mpsc::unbounded_channel();
        let events = Faults(sent);
        let supervising = operations.supervise_ready(
            &route,
            child,
            Box::pin(async move { Ok(control) }),
            &events,
            crate::operations::managed_supervision::signals().unwrap(),
        );
        tokio::pin!(supervising);
        let error = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                exit = &mut supervising => panic!("native terminated on observation error: {exit:?}"),
                error = faults.recv() => error.unwrap(),
            }
        }).await.unwrap();
        assert_eq!(error["details"]["reason"], "native_mcp_runtime_unavailable");
        assert_eq!(error["details"]["runtime_status"], "failed");
        assert_eq!(error["retryable"], false);
        assert_eq!(
            proof.state(),
            agentchat_core::model::LaunchOwnerState::Current
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !seen.exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(registry.root_departed(
            original.context.project_id,
            original.context.host,
            &original.context.host_session_id,
            &original.identity,
        ));
        let descriptors = fs::read_dir(manager.home().join("contexts"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert_eq!(descriptors, 1, "blocked target acquired a descriptor");
        let store = manager.open(&route.project).await.unwrap();
        let statistics = store.snapshot().await.unwrap().statistics().await.unwrap();
        assert_eq!(statistics.agents, 1);
        assert_eq!(statistics.messages, 0);
        assert!(
            serde_json::to_value(&statistics)
                .unwrap()
                .get("acknowledged_deliveries")
                .is_none()
        );
        fs::write(seen.with_extension("finish"), b"normal native exit").unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), &mut supervising)
                .await
                .unwrap()
                .unwrap(),
            0
        );
        handle.shutdown();
        runtime_task.await.unwrap().unwrap();
        server.abort();
        return;
    }
    let mut saw_target_response_gate = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        control.tick(&operations, &route, &proof).await.unwrap();
        if !cross_project
            && control.started.as_ref().is_some_and(|started| {
                started.context.host_session_id == target_root && control.codex_candidate.is_some()
            })
        {
            saw_target_response_gate = true;
            assert!(!registry.was_started(&control.binding).unwrap());
        }
        let finished = if cross_project {
            control.started.is_none() && control.codex_candidate.is_none()
        } else {
            control.started.as_ref().is_some_and(|started| {
                started.context.host_session_id == target_root && control.codex_candidate.is_none()
            })
        };
        if finished {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut client = operations.connect(&route).await.unwrap();
    if cross_project {
        assert_eq!(control.binding.context.context_id, initial);
    } else {
        let target = control.binding.context.context_id;
        assert_ne!(target, initial);
        assert_eq!(
            client.native_binding(target).await.unwrap().participation,
            Participation::Joined
        );
        assert!(saw_target_response_gate);
        assert!(registry.was_started(&control.binding).unwrap());
    }
    assert_eq!(
        client.native_binding(initial).await.unwrap().participation,
        Participation::Left
    );
    assert!(
        seen.exists(),
        "native response was held for AgentChat activation"
    );
    fs::write(seen.with_extension("finish"), b"finish").unwrap();
    assert!(child.wait().await.unwrap().success());
    handle.shutdown();
    tokio::time::timeout(Duration::from_secs(5), runtime_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.abort();
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if predicate() {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn fake_codex(
    listener: UnixListener,
    initial_directory: std::path::PathBuf,
    target_directory: std::path::PathBuf,
    target: String,
    namespace: String,
    failed_mcp: bool,
    home: std::path::PathBuf,
) {
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let initial_directory = initial_directory.clone();
        let target_directory = target_directory.clone();
        let target = target.clone();
        let namespace = namespace.clone();
        let home = home.clone();
        tokio::spawn(async move {
            let mut socket = accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: serde_json::Value = serde_json::from_str(&text).unwrap();
                let Some(id) = request.get("id").cloned() else {
                    continue;
                };
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => serde_json::json!({
                        "userAgent":"codex-cli/synthetic-transition"
                    }),
                    "thread/read" => serde_json::json!({"thread":{
                        "id":request["params"]["threadId"],
                        "cwd":if request["params"]["threadId"] == target {
                            &target_directory
                        } else {
                            &initial_directory
                        },
                        "status":{"type":"idle"}
                    }}),
                    "mcpServerStatus/list" => serde_json::json!({
                        "data":[{"name":"agentchat","runtimeStatus":if failed_mcp {"failed"} else {"connected"},
                        "serverInfo":{"name":"agentchat","version":env!("CARGO_PKG_VERSION")},
                        "serverCapabilities":{"experimental":{"agentchat/native_routing":{
                            "host":"codex",
                            "protocol":agentchat_protocol::NATIVE_ROUTING_PROTOCOL,
                            "namespace":namespace
                        }}},"toolsError":null,"tools":{
                            "agentchat_whoami":{"name":"agentchat_whoami"},
                            "agentchat_post":{"name":"agentchat_post"},
                            "agentchat_read":{"name":"agentchat_read"}
                        }}],"nextCursor":null
                    }),
                    "thread/resume" => {
                        let expected = home.join("expected-switch-binding.json");
                        if expected.exists() {
                            let current: Binding =
                                serde_json::from_slice(&fs::read(expected).unwrap()).unwrap();
                            assert!(
                                Registry::new(&home).root_changed(
                                    current.context.project_id,
                                    current.context.host,
                                    &current.context.host_session_id,
                                    &current.identity
                                ),
                                "complete native request arrived before the current generation was fenced"
                            );
                        }
                        assert!(
                            fs::read_dir(home.join("hosts/launches/root-transitions"))
                                .unwrap()
                                .any(|entry| entry
                                    .unwrap()
                                    .path()
                                    .extension()
                                    .is_some_and(|ext| ext == "json")),
                            "native entered before the durable delivery fence"
                        );
                        serde_json::json!({"thread":{"id":target}})
                    }
                    "thread/unsubscribe" => serde_json::json!({"status":"unsubscribed"}),
                    method => panic!("unexpected fake Codex method {method}"),
                };
                let response = if request["method"] == "thread/resume"
                    && home.join("resume-rejected").exists()
                {
                    serde_json::json!({"id":id,"error":{"code":-32600,"message":"cancelled"}})
                } else {
                    serde_json::json!({"id":id,"result":result})
                };
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .unwrap();
            }
        });
    }
}

#[path = "codex_rejoin.rs"]
mod rejoin;

#[path = "codex_initial_activation.rs"]
mod initial_activation;

#[path = "codex_initial_attachment.rs"]
mod initial_attachment;
