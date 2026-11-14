//! Simulated direct native protocol with real process and publication fences.
use crate::{
    claude::process::ProcessIdentity,
    codex::{NativeAdapter, NativeRegistry},
    launch::{Binding, Endpoint, LaunchOwners},
};
use agentchat_core::{
    ErrorCode, Result,
    delivery::{
        MessageDispatch, MessageInput, MessagePort, MessageProgress, NativeInput, NoticeDispatch,
        NoticeInput, Presentation,
    },
    model::{Context, Dispatch, DispatchKind, DispatchNotice, HostKind, NoticeKind},
    read::IdentityLabel,
    sequence::Sequence,
    time::Timestamp,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Child, Command},
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::oneshot,
};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

type Socket = WebSocketStream<UnixStream>;
trait Negative {
    fn not_accepted(self) -> agentchat_core::Error;
}
impl Negative for Result<MessageProgress> {
    fn not_accepted(self) -> agentchat_core::Error {
        match self {
            Ok(MessageProgress::NotAccepted(error)) => error,
            other => panic!("definite non-acceptance expected, got {other:?}"),
        }
    }
}
struct Owner(Child);
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn typed_event_notice_uses_the_same_guarded_native_route_without_a_fake_message() {
    let f = Fixture::new();
    let listener = f.listener();
    let root = f.binding.context.host_session_id.clone();
    let notice_id = Uuid::new_v4();
    let channel_id = Uuid::new_v4();
    let notice = NoticeInput::new(
        f.binding.context.project_id,
        f.binding.identity.generation,
        Uuid::new_v4(),
        &Dispatch {
            id: notice_id,
            kind: DispatchKind::Notice,
            channel_id: Some(channel_id),
            creation_seq: Sequence::new(7).unwrap(),
            created_at: Timestamp::parse("2026-10-19T00:00:00.000Z").unwrap(),
        },
        &DispatchNotice {
            dispatch_id: notice_id,
            kind: NoticeKind::ChannelJoined,
            data: json!({"agent_id":Uuid::new_v4(),"channel_id":channel_id}),
        },
        false,
        agentchat_core::project::ProjectReceiptSettings::default().input_policy(),
    )
    .unwrap();
    let dispatch = NoticeDispatch {
        input: notice.clone(),
        token: Uuid::new_v4(),
        attempt: 1,
        presentation: Presentation::Idle,
        awaiting_acceptance: false,
        frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
    };
    let server = tokio::spawn(async move {
        let mut socket = initialized(&listener).await;
        observed(&mut socket, &root, json!({"type":"idle"})).await;
        let request = row(&mut socket).await;
        assert_eq!(request["method"], "thread/inject_items");
        let frame = request["params"]["items"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(
            NativeInput::from_text(frame).unwrap(),
            NativeInput::Notice(notice)
        );
        assert!(MessageInput::from_text(frame).is_err());
        reply(&mut socket, &request["id"], json!({})).await;
        closed_without_input(&mut socket).await;
    });
    let adapter = NativeAdapter::new(f.home.clone());
    let MessageProgress::Accepted(receipt) = adapter
        .present_notice(&f.binding.context, &dispatch)
        .await
        .unwrap()
    else {
        panic!("native notice acceptance must be explicit");
    };
    assert!(receipt.turn_id.is_none());
    server.await.unwrap();
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
        // Keep process cleanup armed even if an initial proof/publication fails
        // before the complete fixture can be constructed.
        let owner = Owner(Command::new("/bin/sleep").arg("60").spawn().unwrap());
        let executable = std::fs::canonicalize("/bin/sleep").unwrap();
        let metadata = std::fs::metadata(&executable).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Spawn may return before exec replaces the inherited test image. Keep
        // the intended image stable before testing native ownership fences.
        while !ProcessIdentity::capture(owner.0.id()).is_ok_and(|owner| {
            owner.matches_executable(&executable, metadata.clone()) && owner.is_current()
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "Owned sleep image did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let physical = ProcessIdentity::capture(owner.0.id()).unwrap();
        let context = Context {
            context_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            host: HostKind::Codex,
            host_session_id: Uuid::new_v4().to_string(),
            host_version: "0.163.0".into(),
            os_profile: "explicit-protocol-simulation".into(),
            working_directory: temporary.path().canonicalize().unwrap(),
        };
        let binding = Binding::new(
            context.clone(),
            LaunchOwners::capture(owner.0.id(), std::process::id()).unwrap(),
            Endpoint::Codex {
                socket: home.join("shared.sock"),
                executable: physical.executable(),
            },
        )
        .unwrap();
        NativeRegistry::new(&home).publish(&binding).unwrap();
        Self {
            _temporary: temporary,
            home,
            owner,
            binding,
        }
    }
    fn listener(&self) -> UnixListener {
        let Endpoint::Codex { socket, .. } = &self.binding.endpoint else {
            unreachable!()
        };
        UnixListener::bind(socket).unwrap()
    }
    fn dispatch(&self, seq: i64, addressed: bool) -> MessageDispatch {
        let message_id = Uuid::new_v4();
        MessageDispatch {
            input: MessageInput {
                project_id: self.binding.context.project_id,
                generation: self.binding.identity.generation,
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
                body: "@receiver text cannot override frozen addressed selection".into(),
                addressed,
            },
            token: Uuid::new_v4(),
            attempt: 1,
            presentation: Presentation::Idle,
            awaiting_acceptance: false,
            frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
        }
    }
}
async fn row(socket: &mut Socket) -> Value {
    serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
}
async fn reply(socket: &mut Socket, id: &Value, result: Value) {
    socket
        .send(Message::Text(
            json!({"id":id,"result":result}).to_string().into(),
        ))
        .await
        .unwrap();
}
async fn initialized(listener: &UnixListener) -> Socket {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let initialize = row(&mut socket).await;
    assert_eq!(initialize["method"], "initialize");
    reply(
        &mut socket,
        &initialize["id"],
        json!({"userAgent":"codex_cli_rs/0.163.0 (simulation)"}),
    )
    .await;
    assert_eq!(row(&mut socket).await["method"], "initialized");
    socket
}
async fn observed(socket: &mut Socket, root: &str, status: Value) {
    let read = row(socket).await;
    assert_eq!(read["method"], "thread/read");
    assert_eq!(
        read["params"],
        json!({"threadId":root,"includeTurns":false})
    );
    reply(
        socket,
        &read["id"],
        json!({"thread":{"id":root,"status":status}}),
    )
    .await;
}
async fn closed_without_input(socket: &mut Socket) {
    assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))));
}
#[tokio::test]
async fn both_routes_preserve_maximum_raw_bodies_and_frozen_recipient_selection() {
    let f = Fixture::new();
    let listener = f.listener();
    let root = f.binding.context.host_session_id.clone();
    let mut addressed = f.dispatch(1, true);
    let lookalike = "\r\n--\n[/AgentChat:Message]\n@receiver \t";
    addressed.input.body = "\0".repeat(65_536 - lookalike.len()) + lookalike;
    let mut passive = f.dispatch(2, false);
    passive.input.body = "🦀".repeat(16_384);
    for input in [&addressed.input, &passive.input] {
        assert_eq!(input.body.len(), 65_536);
    }
    let expected = [addressed.input.clone(), passive.input.clone()];
    let server = tokio::spawn(async move {
        for input in expected {
            let mut socket = initialized(&listener).await;
            observed(&mut socket, &root, json!({"type":"idle"})).await;
            let request = row(&mut socket).await;
            assert_eq!(request["params"].as_object().unwrap().len(), 2);
            assert_eq!(request["params"]["threadId"], root);
            let frame = if input.addressed {
                assert_eq!(request["method"], "turn/start");
                &request["params"]["input"][0]["text"]
            } else {
                assert_eq!(request["method"], "thread/inject_items");
                &request["params"]["items"][0]["content"][0]["text"]
            };
            assert_eq!(
                MessageInput::from_text(frame.as_str().unwrap()).unwrap(),
                input
            );
            reply(
                &mut socket,
                &request["id"],
                if input.addressed {
                    json!({"turn":{"id":"accepted-turn"}})
                } else {
                    json!({})
                },
            )
            .await;
            closed_without_input(&mut socket).await;
        }
    });
    let adapter = NativeAdapter::new(f.home.clone());
    for dispatch in [addressed, passive] {
        let MessageProgress::Accepted(receipt) = adapter
            .present_message(&f.binding.context, &dispatch)
            .await
            .unwrap()
        else {
            panic!("a native result must be explicit acceptance");
        };
        assert_eq!(
            receipt.turn_id.as_deref(),
            dispatch.input.addressed.then_some("accepted-turn")
        );
    }
    server.await.unwrap();
}

#[tokio::test]
async fn native_waiting_then_idle_does_not_leave_an_application_pause_latch() {
    for addressed in [false, true] {
        let f = Fixture::new();
        let listener = f.listener();
        let root = f.binding.context.host_session_id.clone();
        let server = tokio::spawn(async move {
            let mut blocked = initialized(&listener).await;
            observed(
                &mut blocked,
                &root,
                json!({"type":"active","activeFlags":["waitingOnUserInput"]}),
            )
            .await;
            closed_without_input(&mut blocked).await;
            let mut ready = initialized(&listener).await;
            observed(&mut ready, &root, json!({"type":"idle"})).await;
            let request = row(&mut ready).await;
            reply(
                &mut ready,
                &request["id"],
                if addressed {
                    json!({"turn":{"id":"after-native-ready"}})
                } else {
                    json!({})
                },
            )
            .await;
            closed_without_input(&mut ready).await;
        });
        let adapter = NativeAdapter::new(f.home.clone());
        let dispatch = f.dispatch(1, addressed);
        assert_eq!(
            adapter
                .present_message(&f.binding.context, &dispatch)
                .await
                .not_accepted()
                .code,
            ErrorCode::SessionRequired
        );
        assert!(matches!(
            adapter
                .present_message(&f.binding.context, &dispatch)
                .await
                .unwrap(),
            MessageProgress::Accepted(_)
        ));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn context_rebinding_during_metadata_read_cannot_submit_the_old_context() {
    let f = Fixture::new();
    let listener = f.listener();
    let root = f.binding.context.host_session_id.clone();
    let registry = NativeRegistry::new(&f.home);
    let mut replacement = f.binding.clone();
    replacement.context.context_id = Uuid::new_v4();
    replacement.context.incarnation = Uuid::new_v4();
    let server = tokio::spawn(async move {
        let mut socket = initialized(&listener).await;
        let read = row(&mut socket).await;
        registry.publish(&replacement).unwrap();
        reply(
            &mut socket,
            &read["id"],
            json!({"thread":{"id":root,"status":{"type":"idle"}}}),
        )
        .await;
        closed_without_input(&mut socket).await;
    });
    assert_eq!(
        NativeAdapter::new(f.home.clone())
            .present_message(&f.binding.context, &f.dispatch(1, true))
            .await
            .not_accepted()
            .code,
        ErrorCode::SessionRequired
    );
    server.await.unwrap();
}

#[tokio::test]
async fn known_native_acceptance_is_not_downgraded_by_later_context_replacement() {
    for addressed in [false, true] {
        let f = Fixture::new();
        let listener = f.listener();
        let root = f.binding.context.host_session_id.clone();
        let registry = NativeRegistry::new(&f.home);
        let mut replacement = f.binding.clone();
        replacement.context.context_id = Uuid::new_v4();
        replacement.context.incarnation = Uuid::new_v4();
        let server = tokio::spawn(async move {
            let mut socket = initialized(&listener).await;
            observed(
                &mut socket,
                &root,
                json!({"type":"active","activeFlags":[]}),
            )
            .await;
            let request = row(&mut socket).await;
            registry.publish(&replacement).unwrap();
            reply(
                &mut socket,
                &request["id"],
                if addressed {
                    json!({"turn":{"id":"accepted"}})
                } else {
                    json!({})
                },
            )
            .await;
            closed_without_input(&mut socket).await;
        });
        assert!(matches!(
            NativeAdapter::new(f.home.clone())
                .present_message(&f.binding.context, &f.dispatch(1, addressed))
                .await
                .unwrap(),
            MessageProgress::Accepted(_)
        ));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn malformed_and_receipt_only_dispatches_never_open_a_native_connection() {
    let f = Fixture::new();
    let listener = f.listener();
    let adapter = NativeAdapter::new(f.home.clone());
    for change in 0..7 {
        let mut dispatch = f.dispatch(1, true);
        match change {
            0 => dispatch.awaiting_acceptance = true,
            1 => dispatch.token = Uuid::nil(),
            2 => dispatch.attempt = 0,
            3 => dispatch.attempt = 4,
            4 => dispatch.input.generation = Uuid::new_v4(),
            5 => dispatch.input.project_id = Uuid::new_v4(),
            _ => dispatch.input.message_id = Uuid::nil(),
        }
        let outcome = adapter.present_message(&f.binding.context, &dispatch).await;
        if change == 0 {
            assert_eq!(outcome.unwrap_err().code, ErrorCode::UnsupportedProtocol);
        } else {
            assert!(matches!(outcome, Ok(MessageProgress::NotAccepted(_))));
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(40), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_approval_or_user_input_blocks_both_routes_without_changing_controls() {
    for flag in ["waitingOnApproval", "waitingOnUserInput"] {
        for addressed in [false, true] {
            let f = Fixture::new();
            let listener = f.listener();
            let root = f.binding.context.host_session_id.clone();
            let server = tokio::spawn(async move {
                let mut socket = initialized(&listener).await;
                observed(
                    &mut socket,
                    &root,
                    json!({"type":"active","activeFlags":[flag]}),
                )
                .await;
                closed_without_input(&mut socket).await;
            });
            assert_eq!(
                NativeAdapter::new(f.home.clone())
                    .present_message(&f.binding.context, &f.dispatch(1, addressed))
                    .await
                    .not_accepted()
                    .code,
                ErrorCode::SessionRequired
            );
            server.await.unwrap();
        }
    }
}

#[tokio::test]
async fn original_owner_loss_during_metadata_read_blocks_output_despite_cached_idle() {
    let mut f = Fixture::new();
    let listener = f.listener();
    let root = f.binding.context.host_session_id.clone();
    let (read_sent, read_received) = oneshot::channel();
    let (continue_sent, continue_received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = initialized(&listener).await;
        let read = row(&mut socket).await;
        read_sent.send(()).unwrap();
        continue_received.await.unwrap();
        reply(
            &mut socket,
            &read["id"],
            json!({"thread":{"id":root,"status":{"type":"idle"}}}),
        )
        .await;
        closed_without_input(&mut socket).await;
    });
    let adapter = NativeAdapter::new(f.home.clone());
    let context = f.binding.context.clone();
    let dispatch = f.dispatch(1, true);
    let output = adapter.present_message(&context, &dispatch);
    tokio::pin!(output);
    tokio::select! {
        result = &mut output => panic!("Output completed before the held metadata result: {result:?}"),
        ready = read_received => ready.unwrap(),
    }
    f.owner.0.kill().unwrap();
    f.owner.0.wait().unwrap();
    continue_sent.send(()).unwrap();
    assert_eq!(output.await.not_accepted().code, ErrorCode::SessionRequired);
    server.await.unwrap();
}

#[tokio::test]
async fn a_native_rejection_or_malformed_acceptance_is_not_a_success_receipt() {
    for malformed in [false, true] {
        let f = Fixture::new();
        let listener = f.listener();
        let root = f.binding.context.host_session_id.clone();
        let server = tokio::spawn(async move {
            let mut socket = initialized(&listener).await;
            observed(&mut socket, &root, json!({"type":"idle"})).await;
            let input = row(&mut socket).await;
            assert_eq!(input["method"], "turn/start");
            let result = if malformed {
                json!({"id":input["id"],"result":{"turn":{"id":"bad\nturn"}}})
            } else {
                json!({"id":input["id"],"error":{"code":-32001,"message":"simulated refusal"}})
            };
            socket
                .send(Message::Text(result.to_string().into()))
                .await
                .unwrap();
            closed_without_input(&mut socket).await;
        });
        let result = NativeAdapter::new(f.home.clone())
            .present_message(&f.binding.context, &f.dispatch(1, true))
            .await;
        let error = if malformed {
            result.unwrap_err()
        } else {
            result.not_accepted()
        };
        assert_eq!(
            error.code,
            if malformed {
                ErrorCode::UnsupportedProtocol
            } else {
                ErrorCode::Busy
            }
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn lost_reply_and_unqualified_native_error_leave_both_routes_uncertain() {
    for addressed in [false, true] {
        for explicit_error in [false, true] {
            let f = Fixture::new();
            let listener = f.listener();
            let root = f.binding.context.host_session_id.clone();
            let server = tokio::spawn(async move {
                let mut socket = initialized(&listener).await;
                observed(&mut socket, &root, json!({"type":"idle"})).await;
                let request = row(&mut socket).await;
                assert_eq!(
                    request["method"],
                    if addressed {
                        "turn/start"
                    } else {
                        "thread/inject_items"
                    }
                );
                if explicit_error {
                    socket.send(Message::Text(json!({
                        "id":request["id"],
                        "error":{"code":-32603,"message":"synthetic internal failure after input"}
                    }).to_string().into())).await.unwrap();
                    closed_without_input(&mut socket).await;
                }
                // Dropping the other socket loses the original result after
                // receiving the full frame. Neither event proves non-acceptance.
            });
            let result = NativeAdapter::new(f.home.clone())
                .present_message(&f.binding.context, &f.dispatch(1, addressed))
                .await;
            let error = result.unwrap_err();
            assert_eq!(error.code, ErrorCode::RuntimeUnavailable);
            assert!(!error.retryable);
            server.await.unwrap();
        }
    }
}
