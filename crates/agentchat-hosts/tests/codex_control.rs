//! Real Unix/WebSocket transport with simulated native responses, no admission.
use agentchat_core::{
    ErrorCode,
    delivery::MessageInput,
    model::{Context, HostKind},
    read::IdentityLabel,
    sequence::Sequence,
};
use agentchat_hosts::{codex::Client, launch::LaunchOwners};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command},
    time::Duration,
};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
use uuid::Uuid;
type Socket = WebSocketStream<UnixStream>;
struct Owned(Child);
impl Owned {
    fn new() -> Self {
        let child = Self(Command::new("/bin/sleep").arg("60").spawn().unwrap());
        let executable = std::fs::canonicalize("/bin/sleep").unwrap();
        let metadata = std::fs::metadata(&executable).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Spawn may return before exec replaces the inherited test image. Keep
        // the intended image stable before testing native ownership fences.
        while !agentchat_hosts::claude::process::ProcessIdentity::capture(child.0.id()).is_ok_and(
            |owner| owner.matches_executable(&executable, metadata.clone()) && owner.is_current(),
        ) {
            assert!(
                std::time::Instant::now() < deadline,
                "Owned sleep image did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        child
    }
    fn identity(&self) -> agentchat_core::model::LaunchIdentity {
        LaunchOwners::capture(self.0.id(), std::process::id()).unwrap()
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn row(s: &mut Socket) -> Value {
    serde_json::from_str(s.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
}
async fn reply(s: &mut Socket, id: &Value, result: Value) {
    s.send(Message::Text(
        json!({"id":id,"result":result}).to_string().into(),
    ))
    .await
    .unwrap();
}
async fn initialize(listener: UnixListener) -> Socket {
    let (stream, _) = listener.accept().await.unwrap();
    let mut s = tokio_tungstenite::accept_async(stream).await.unwrap();
    let init = row(&mut s).await;
    assert_eq!(init["method"], "initialize");
    assert_eq!(init["params"]["capabilities"]["experimentalApi"], true);
    reply(
        &mut s,
        &init["id"],
        json!({"userAgent":"codex_cli_rs/0.163.0 (native)"}),
    )
    .await;
    assert_eq!(
        row(&mut s).await,
        json!({"method":"initialized","params":{}})
    );
    s
}
fn fixture() -> (TempDir, PathBuf, UnixListener) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    (root, path, listener)
}
fn context() -> Context {
    Context {
        context_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        host: HostKind::Codex,
        host_session_id: "exact-native-root".into(),
        host_version: "0.163.0".into(),
        os_profile: "synthetic-test".into(),
        working_directory: PathBuf::from("/tmp"),
        incarnation: Uuid::new_v4(),
    }
}
fn message(context: &Context, generation: Uuid, seq: i64, addressed: bool) -> MessageInput {
    let message_id = Uuid::new_v4();
    MessageInput {
        project_id: context.project_id,
        generation,
        delivery_id: Uuid::new_v4(),

        auto_read: true,
        frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
        dispatch_id: message_id,
        seq: Sequence::new(seq).unwrap(),
        message_id,
        thread_id: Uuid::new_v4(),
        author: IdentityLabel {
            actor_id: Uuid::new_v4(),
            kind: agentchat_core::model::ActorKind::Agent,
            handle: "sender".into(),
        },
        channel_id: Uuid::new_v4(),
        channel: "dev".into(),
        body: "complete original body".into(),
        addressed,
    }
}
fn text(input: &Value) -> MessageInput {
    let text = input.as_str().unwrap();
    MessageInput::from_text(text).unwrap()
}
#[tokio::test]
async fn alternating_full_bodies_share_one_serial_stream_without_host_overrides() {
    let (_root, path, listener) = fixture();
    let owner = Owned::new();
    let identity = owner.identity();
    let context = context();
    let mut inputs = (1..=3)
        .map(|i| message(&context, identity.generation, i, i != 2))
        .collect::<Vec<_>>();
    inputs[0].body = "\"\\\u{1}".repeat(21_845) + "x";
    assert_eq!(inputs[0].body.len(), 65_536);
    let expected = inputs.clone();
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        for (i, input) in expected.into_iter().enumerate() {
            let observe = row(&mut s).await;
            assert_eq!(observe["method"], "thread/read");
            reply(
                &mut s,
                &observe["id"],
                json!({"thread":{"id":"exact-native-root","status":{"type":"idle"}}}),
            )
            .await;
            let request = row(&mut s).await;
            assert_eq!(request["params"].as_object().unwrap().len(), 2);
            assert_eq!(request["params"]["threadId"], "exact-native-root");
            let delivered = if i == 1 {
                assert_eq!(request["method"], "thread/inject_items");
                text(&request["params"]["items"][0]["content"][0]["text"])
            } else {
                assert_eq!(request["method"], "turn/start");
                text(&request["params"]["input"][0]["text"])
            };
            assert_eq!(delivered, input);
            // No second request may precede the first native result.
            assert!(
                tokio::time::timeout(Duration::from_millis(40), s.next())
                    .await
                    .is_err()
            );
            reply(
                &mut s,
                &request["id"],
                if i == 1 {
                    json!({})
                } else {
                    json!({"turn":{"id":format!("turn-{i}")}})
                },
            )
            .await;
        }
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    for input in inputs {
        client.deliver(&context, &identity, &input).await.unwrap();
    }
    server.await.unwrap();
}
#[tokio::test]
async fn a_rejected_head_retires_the_connection_and_never_sends_a_successor() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        let request = row(&mut s).await;
        assert_eq!(request["method"], "thread/read");
        s.send(Message::Text(
            json!({"id":request["id"],"error":{"code":-32001,"message":"overload"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert!(!matches!(s.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    assert_eq!(
        client.observe("exact-root").await.unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(
        client.observe("later-root").await.unwrap_err().code,
        ErrorCode::RuntimeUnavailable
    );
    server.await.unwrap();
}
#[tokio::test]
async fn version_mismatch_never_initializes_or_issues_a_thread_operation() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut s = tokio_tungstenite::accept_async(stream).await.unwrap();
        let init = row(&mut s).await;
        reply(
            &mut s,
            &init["id"],
            json!({"userAgent":"codex_cli_rs/0.999.0"}),
        )
        .await;
        assert!(!matches!(s.next().await, Some(Ok(Message::Text(_)))));
    });
    assert!(
        matches!(Client::connect(&path,"0.163.0").await,Err(e) if e.code==ErrorCode::UnsupportedHost)
    );
    server.await.unwrap();
}

#[tokio::test]
async fn missing_initialize_method_does_not_invent_an_observed_version() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let initialize = row(&mut socket).await;
        assert_eq!(initialize["method"], "initialize");
        socket
            .send(Message::Text(
                json!({"id":initialize["id"],"error":{"code":-32601,"message":"Method not found"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let error = Client::connect(&path, "0.164.7").await.err().unwrap();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["host"], "codex");
    assert_eq!(error.details["host_version"], "0.164.7");
    assert!(error.details["observed_version"].is_null());
    assert_eq!(error.details["missing_api"], "initialize");
    assert_eq!(error.details["api_error_code"], -32601);
    assert!(!error.retryable);
    server.await.unwrap();
}

#[tokio::test]
async fn missing_initialization_user_agent_names_the_pointer_and_closes_the_connection() {
    for result in [
        json!({}),
        json!({"userAgent":42}),
        json!({"userAgent":""}),
        json!({"userAgent":"   "}),
        json!({"userAgent":"codex_cli_rs/"}),
    ] {
        let (_root, path, listener) = fixture();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = row(&mut socket).await;
            assert_eq!(initialize["method"], "initialize");
            reply(&mut socket, &initialize["id"], result).await;
            assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
        });
        let error = Client::connect(&path, "0.163.0").await.err().unwrap();
        assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
        assert_eq!(error.details["host"], "codex");
        assert_eq!(error.details["host_version"], "0.163.0");
        assert!(error.details["observed_version"].is_null());
        assert_eq!(error.details["missing_api"], "initialize.result.userAgent");
        assert!(!error.retryable);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn missing_initialize_result_names_the_envelope_without_an_observed_version() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let initialize = row(&mut socket).await;
        assert_eq!(initialize["method"], "initialize");
        socket
            .send(Message::Text(
                json!({"id":initialize["id"]}).to_string().into(),
            ))
            .await
            .unwrap();
        assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
    });
    let error = Client::connect(&path, "0.163.0").await.err().unwrap();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["host"], "codex");
    assert_eq!(error.details["host_version"], "0.163.0");
    assert!(error.details["observed_version"].is_null());
    assert_eq!(error.details["missing_api"], "initialize.result");
    assert!(!error.retryable);
    server.await.unwrap();
}

#[tokio::test]
async fn missing_later_result_names_the_method_envelope() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let mut socket = initialize(listener).await;
        let request = row(&mut socket).await;
        assert_eq!(request["method"], "thread/read");
        socket
            .send(Message::Text(
                json!({"id":request["id"]}).to_string().into(),
            ))
            .await
            .unwrap();
        assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    let error = client.observe("exact-root").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["host"], "codex");
    assert_eq!(error.details["host_version"], "0.163.0");
    assert_eq!(error.details["observed_version"], "0.163.0");
    assert_eq!(error.details["missing_api"], "thread/read.result");
    assert!(!error.retryable);
    server.await.unwrap();
}

#[tokio::test]
async fn another_matching_version_connects_and_reports_a_missing_required_api() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let initialize = row(&mut socket).await;
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(
            initialize["params"]["capabilities"]["experimentalApi"],
            true
        );
        reply(
            &mut socket,
            &initialize["id"],
            json!({"userAgent":"codex_cli_rs/0.164.7 (native)"}),
        )
        .await;
        assert_eq!(
            row(&mut socket).await,
            json!({"method":"initialized","params":{}})
        );
        let request = row(&mut socket).await;
        assert_eq!(request["method"], "thread/read");
        socket
            .send(Message::Text(
                json!({"id":request["id"],"error":{"code":-32601,"message":"Method not found"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.164.7").await.unwrap();
    let error = client.observe("exact-root").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["host"], "codex");
    assert_eq!(error.details["host_version"], "0.164.7");
    assert_eq!(error.details["observed_version"], "0.164.7");
    assert_eq!(error.details["missing_api"], "thread/read");
    assert_eq!(error.details["api_error_code"], -32601);
    assert!(!error.retryable);
    server.await.unwrap();
}

#[tokio::test]
async fn changed_required_api_shape_is_reported_to_the_caller() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let mut socket = initialize(listener).await;
        let request = row(&mut socket).await;
        assert_eq!(request["method"], "thread/read");
        socket
            .send(Message::Text(
                json!({"id":request["id"],"error":{"code":-32602,"message":"Invalid params"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    let error = client.observe("exact-root").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["missing_api"], "thread/read");
    assert_eq!(error.details["api_error_code"], -32602);
    assert!(!error.retryable);
    server.await.unwrap();
}

#[tokio::test]
async fn creating_a_root_uses_defaults_and_only_one_passive_non_model_seed() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        let start = row(&mut s).await;
        assert_eq!(start["method"], "thread/start");
        assert_eq!(start["params"], json!({"cwd":"/tmp"}));
        reply(
            &mut s,
            &start["id"],
            json!({"thread":{"id":"new-exact-root"}}),
        )
        .await;
        let inject = row(&mut s).await;
        assert_eq!(inject["method"], "thread/inject_items");
        assert_eq!(inject["params"]["threadId"], "new-exact-root");
        assert!(
            inject["params"]["items"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("No task is requested")
        );
        reply(&mut s, &inject["id"], json!({})).await;
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    assert_eq!(
        client
            .create_thread(std::path::Path::new("/tmp"))
            .await
            .unwrap(),
        "new-exact-root"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn a_malformed_native_acceptance_retires_the_connection_before_any_successor() {
    let (_root, path, listener) = fixture();
    let owner = Owned::new();
    let identity = owner.identity();
    let context = context();
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        let observe = row(&mut s).await;
        reply(&mut s, &observe["id"], json!({"thread":{"id":"exact-native-root","status":{"type":"active","activeFlags":[]}}})).await;
        let body = row(&mut s).await;
        assert_eq!(body["method"], "turn/start");
        reply(&mut s, &body["id"], json!({"turn":{}})).await;
        assert!(!matches!(s.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    let error = client
        .deliver(
            &context,
            &identity,
            &message(&context, identity.generation, 1, true),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(error.details["host"], "codex");
    assert_eq!(error.details["missing_api"], "turn/start.result.turn.id");
    assert_eq!(
        client
            .deliver(
                &context,
                &identity,
                &message(&context, identity.generation, 2, false)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::RuntimeUnavailable
    );
    server.await.unwrap();
}
#[tokio::test]
async fn native_approval_requests_are_never_answered_by_the_control_client() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        let _request = row(&mut s).await;
        s.send(Message::Text(
            json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert!(!matches!(s.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    assert_eq!(
        client.observe("root").await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    server.await.unwrap();
}
#[tokio::test]
async fn native_phase_and_exact_root_checks_do_not_supply_launch_liveness() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        for status in [
            json!({"type":"idle"}),
            json!({"type":"active","activeFlags":["waitingOnApproval"]}),
            json!({"type":"active","activeFlags":["waitingOnUserInput"]}),
            json!({"type":"notLoaded"}),
        ] {
            let request = row(&mut s).await;
            assert_eq!(
                request["params"],
                json!({"threadId":"root","includeTurns":false})
            );
            reply(
                &mut s,
                &request["id"],
                json!({"thread":{"id":"root","status":status}}),
            )
            .await;
        }
        let request = row(&mut s).await;
        reply(
            &mut s,
            &request["id"],
            json!({"thread":{"id":"foreign-root","status":{"type":"idle"}}}),
        )
        .await;
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    for phase in [
        agentchat_core::model::Phase::Idle,
        agentchat_core::model::Phase::ApprovalBlocked,
        agentchat_core::model::Phase::Paused,
        agentchat_core::model::Phase::Disconnected,
    ] {
        assert_eq!(client.observe("root").await.unwrap().phase, phase);
    }
    assert_eq!(
        client.observe("root").await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    server.await.unwrap();
}
#[tokio::test]
async fn blocked_native_phase_cannot_submit_a_body() {
    let (_root, path, listener) = fixture();
    let owner = Owned::new();
    let identity = owner.identity();
    let context = context();
    let input = message(&context, identity.generation, 1, true);
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        let observe = row(&mut s).await;
        assert_eq!(observe["method"], "thread/read");
        reply(&mut s,&observe["id"],json!({"thread":{"id":"exact-native-root","status":{"type":"active","activeFlags":["waitingOnApproval"]}}})).await;
        assert!(!matches!(s.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    assert_eq!(
        client
            .deliver(&context, &identity, &input)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    drop(client);
    server.await.unwrap();
}

#[tokio::test]
async fn lost_original_owner_cannot_submit_on_a_healthy_shared_control_connection() {
    let (_root, path, listener) = fixture();
    let mut owner = Owned::new();
    let identity = owner.identity();
    let context = context();
    let input = message(&context, identity.generation, 1, true);
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        assert!(!matches!(s.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    assert_eq!(
        client
            .deliver(&context, &identity, &input)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    drop(client);
    server.await.unwrap();
}
#[tokio::test]
async fn cancellation_after_native_submission_retires_the_ambiguous_stream() {
    let (_root, path, listener) = fixture();
    let (sent, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut s = initialize(listener).await;
        let request = row(&mut s).await;
        assert_eq!(request["method"], "thread/read");
        sent.send(()).unwrap();
        assert!(!matches!(s.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    let mut request = Box::pin(client.observe("ambiguous-head"));
    tokio::time::timeout(Duration::from_secs(2),async { tokio::select! { result=&mut request => panic!("request unexpectedly completed: {result:?}"), _=received=>{} } }).await.unwrap();
    drop(request);
    assert_eq!(
        client.observe("successor").await.unwrap_err().code,
        ErrorCode::RuntimeUnavailable
    );
    server.await.unwrap();
}

#[tokio::test]
async fn scoped_mcp_health_requires_current_runtime_catalog_and_image_version() {
    let (_root, path, listener) = fixture();
    let server = tokio::spawn(async move {
        let mut socket = initialize(listener).await;
        let healthy = json!({"data":[{"name":"agentchat","runtimeStatus":"connected","serverInfo":{"name":"agentchat","version":"0.1.0"},"serverCapabilities":{"experimental":{"agentchat/native_routing":{"host":"codex","protocol":3,"namespace":"expected"}}},"toolsError":null,"tools":{"agentchat_whoami":{"name":"agentchat_whoami"},"agentchat_post":{"name":"agentchat_post"},"agentchat_read":{"name":"agentchat_read"}}}],"nextCursor":null});
        for change in 0..11 {
            let request = row(&mut socket).await;
            assert_eq!(request["method"], "mcpServerStatus/list");
            assert_eq!(
                request["params"],
                json!({"threadId":"root","serverName":"agentchat","detail":"toolsAndAuthOnly","limit":1})
            );
            let mut result = healthy.clone();
            match change {
                0 => {}
                1 => result["data"][0]["runtimeStatus"] = json!("starting"),
                2 => result["data"][0]["runtimeStatus"] = json!("notStarted"),
                3 => result["data"][0]["serverInfo"]["version"] = json!("0.0.1"),
                4 => result["data"][0]["toolsError"] = json!("unavailable"),
                5 => result["data"][0]["tools"]["agentchat_read"] = json!({"name":"foreign"}),
                6 => result["data"] = json!([]),
                7 => result["data"][0]["serverCapabilities"] = json!({"tools":{}}),
                8 => {
                    result["data"][0]["serverCapabilities"]["experimental"]["agentchat/native_routing"]
                        ["host"] = json!("claude")
                }
                9 => result["data"][0]["serverCapabilities"]["experimental"]["agentchat/native_routing"].as_object_mut().unwrap().remove("namespace").map(|_| ()).unwrap(),
                _ => result["data"][0]["serverCapabilities"]["experimental"]["agentchat/native_routing"]["namespace"] = json!("another-home"),
            }
            reply(&mut socket, &request["id"], result).await;
        }
        let request = row(&mut socket).await;
        reply(
            &mut socket,
            &request["id"],
            json!({"data":[{"name":"foreign"}]}),
        )
        .await;
        assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
    });
    let mut client = Client::connect(&path, "0.163.0").await.unwrap();
    assert!(
        client
            .integration_ready_for("root", "expected")
            .await
            .unwrap()
    );
    for _ in 0..10 {
        assert!(
            !client
                .integration_ready_for("root", "expected")
                .await
                .unwrap()
        );
    }
    assert!(
        client
            .integration_ready_for("root", "expected")
            .await
            .is_err()
    );
    assert!(client.integration_ready("successor").await.is_err());
    drop(client);
    server.await.unwrap();
}

#[tokio::test]
async fn terminal_native_mcp_failure_names_recovery_and_retires_its_client_stream() {
    for state in ["failed", "cancelled", "disabled", "authenticationRequired"] {
        let (_root, path, listener) = fixture();
        let server = tokio::spawn(async move {
            let mut socket = initialize(listener).await;
            let request = row(&mut socket).await;
            assert_eq!(request["method"], "mcpServerStatus/list");
            assert_eq!(request["params"]["threadId"], "retained-root");
            reply(
                &mut socket,
                &request["id"],
                json!({
                    "data":[{"name":"agentchat","runtimeStatus":state}],"nextCursor":null
                }),
            )
            .await;
            assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
        });
        let mut client = Client::connect(&path, "0.163.0").await.unwrap();
        let error = client
            .integration_ready_for("retained-root", "expected")
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigurationConflict);
        assert_eq!(error.details["runtime_status"], state);
        assert_eq!(error.details["reason"], "native_mcp_runtime_unavailable");
        assert!(!error.retryable);
        assert!(client.integration_ready("successor").await.is_err());
        drop(client);
        server.await.unwrap();
    }
}

#[tokio::test]
#[ignore = "manual exact shared-daemon handshake only; requires AGENTCHAT_CODEX_PROBE_CLI and starts no thread/model"]
async fn actual_shared_daemon_version_and_connection() {
    let cli = std::env::var_os("AGENTCHAT_CODEX_PROBE_CLI").expect("explicit native probe CLI");
    let daemon = agentchat_hosts::codex::Daemon::inspect(std::path::Path::new(&cli))
        .await
        .unwrap();
    let client = Client::connect(&daemon.socket, &daemon.version)
        .await
        .unwrap();
    assert_eq!(client.version(), daemon.version);
    println!(
        "existing shared server {}, real binary {}",
        client.version(),
        daemon.executable.display()
    );
}

#[tokio::test]
async fn direct_startup_requires_selected_native_scope_and_owned_mcp_health() {
    for case in 0..10 {
        let (_temporary, path, listener) = fixture();
        let server = tokio::spawn(async move {
            let mut socket = initialize(listener).await;
            let read = row(&mut socket).await;
            assert_eq!(read["method"], "thread/read");
            assert_eq!(
                read["params"],
                json!({"threadId":"root","includeTurns":false})
            );
            let mut value = json!({"thread":{"id":"root","cwd":"/tmp","status":{"type":"idle"}}});
            match case {
                1 => value["thread"]["id"] = json!("another-root"),
                2 => value["thread"]["cwd"] = json!("/another-project"),
                3 => value["thread"]["status"] = json!({"type":"notLoaded"}),
                4 => {
                    value["thread"].as_object_mut().unwrap().remove("cwd");
                }
                6 => {
                    value["thread"]["status"] =
                        json!({"type":"active","activeFlags":["waitingOnApproval"]})
                }
                8 => {
                    value["thread"].as_object_mut().unwrap().remove("status");
                }
                9 => {
                    value["thread"]["status"] =
                        json!({"type":"active","activeFlags":["futureControl"]})
                }
                _ => {}
            }
            reply(&mut socket, &read["id"], value).await;
            if matches!(case, 0 | 5 | 6 | 7) {
                let health = row(&mut socket).await;
                assert_eq!(health["method"], "mcpServerStatus/list");
                assert_eq!(
                    health["params"],
                    json!({"threadId":"root","serverName":"agentchat","detail":"toolsAndAuthOnly","limit":1})
                );
                reply(&mut socket, &health["id"], json!({
                    "data":[{"name":"agentchat","runtimeStatus":"connected",
                    "serverInfo":{"name":"agentchat","version":"0.1.0"},
                    "serverCapabilities":{"experimental":{"agentchat/native_routing":{
                        "host":"codex","protocol":if case==7 {2} else {3},"namespace":if case==5 {"other-state"} else {"selected-state"}
                    }}},"toolsError":null,"tools":{
                        "agentchat_whoami":{"name":"agentchat_whoami"},"agentchat_post":{"name":"agentchat_post"},"agentchat_read":{"name":"agentchat_read"}
                    }}],"nextCursor":null
                })).await;
            }
            assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
        });
        let mut client = Client::connect(&path, "0.163.0").await.unwrap();
        let result = client
            .ready("root", std::path::Path::new("/tmp"), "selected-state")
            .await;
        if matches!(case, 0 | 6) {
            let ready = result.unwrap();
            assert_eq!(ready.root(), "root");
            assert_eq!(ready.directory(), std::path::Path::new("/tmp"));
            assert_eq!(ready.version(), "0.163.0");
            assert_eq!(
                ready.state().phase,
                if case == 6 {
                    agentchat_core::model::Phase::ApprovalBlocked
                } else {
                    agentchat_core::model::Phase::Idle
                }
            );
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                error.code,
                match case {
                    1 => ErrorCode::SessionRequired,
                    4 | 8 | 9 => ErrorCode::UnsupportedProtocol,
                    7 => ErrorCode::ConfigurationConflict,
                    _ => ErrorCode::Busy,
                }
            );
            if matches!(case, 4 | 8 | 9) {
                assert_eq!(
                    error.details["missing_api"],
                    if case == 4 {
                        "thread/read.result.thread.cwd"
                    } else {
                        "thread/read.result.thread.status"
                    }
                );
            }
        }
        drop(client);
        server.await.unwrap();
    }
}
