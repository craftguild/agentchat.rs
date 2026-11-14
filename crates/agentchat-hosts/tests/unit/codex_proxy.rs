use super::*;
use crate::launch::NativeChild;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{collections::BTreeMap, ffi::OsString, fs, os::unix::fs::PermissionsExt};
use tokio::net::{UnixListener, UnixStream};
use tokio_tungstenite::{
    accept_async, client_async,
    tungstenite::{Message, Utf8Bytes},
};

#[test]
fn candidate_metadata_parsing_never_retains_payload_bodies() {
    let resume = Message::Text(Utf8Bytes::from_static(
        r#"{"id":7,"method":"thread/resume","params":{"threadId":"root-a","prompt":"private"}}"#,
    ));
    assert_eq!(
        candidate_request(&resume).unwrap(),
        Some(("7".into(), Some("root-a".into())))
    );
    let start = Message::Text(Utf8Bytes::from_static(
        r#"{"id":"next","method":"thread/start","params":{"cwd":"/private/project"}}"#,
    ));
    assert_eq!(
        candidate_request(&start).unwrap(),
        Some((r#""next""#.into(), None))
    );
    let durable_start = Message::Text(Utf8Bytes::from_static(
        r#"{"id":"durable","method":"thread/start","params":{"ephemeral":false}}"#,
    ));
    assert_eq!(
        candidate_request(&durable_start).unwrap(),
        Some((r#""durable""#.into(), None))
    );
    let internal_start = Message::Text(Utf8Bytes::from_static(
        r#"{"id":"temporary-structured","method":"thread/start","params":{"ephemeral":true,"input":"private"}}"#,
    ));
    assert!(candidate_request(&internal_start).unwrap().is_none());
    let malformed_start = Message::Text(Utf8Bytes::from_static(
        r#"{"id":"malformed","method":"thread/start","params":{"ephemeral":"true"}}"#,
    ));
    assert_eq!(
        candidate_request(&malformed_start).unwrap(),
        Some((r#""malformed""#.into(), None))
    );
    let unrelated = Message::Text(Utf8Bytes::from_static(
        r#"{"id":8,"method":"turn/start","params":{"input":"private"}}"#,
    ));
    assert!(candidate_request(&unrelated).unwrap().is_none());
    for params in [
        serde_json::json!({}),
        serde_json::json!({"threadId": ""}),
        serde_json::json!({"threadId": 7}),
        serde_json::json!({"threadId": "root\nother"}),
    ] {
        let malformed = Message::Text(
            serde_json::json!({"id":7,"method":"thread/resume","params":params})
                .to_string()
                .into(),
        );
        assert!(candidate_request(&malformed).is_err());
    }
}

#[test]
fn only_matching_success_can_supply_a_transition_root() {
    let success = Message::Text(Utf8Bytes::from_static(
        r#"{"id":7,"result":{"thread":{"id":"root-b"}}}"#,
    ));
    assert_eq!(
        candidate_response(&success, |id| id == "7").unwrap(),
        Some(("7".into(), CandidateOutcome::Succeeded("root-b".into())))
    );
    let failure = Message::Text(Utf8Bytes::from_static(
        r#"{"id":7,"error":{"code":-32600,"message":"failed"}}"#,
    ));
    assert_eq!(
        candidate_response(&failure, |id| id == "7").unwrap(),
        Some(("7".into(), CandidateOutcome::Failed))
    );
    let server_request = Message::Text(Utf8Bytes::from_static(
        r#"{"id":7,"method":"item/tool/requestUserInput","result":{"thread":{"id":"unrelated"}}}"#,
    ));
    assert!(
        candidate_response(&server_request, |id| id == "7")
            .unwrap()
            .is_none()
    );
    let indeterminate = Message::Text(Utf8Bytes::from_static(
        r#"{"id":7,"result":{"thread":{"displayName":"changed shape"}}}"#,
    ));
    assert_eq!(
        candidate_response(&indeterminate, |id| id == "7").unwrap(),
        Some(("7".into(), CandidateOutcome::Indeterminate))
    );
}

#[tokio::test]
#[ignore = "owned frontend helper invoked by native_success_is_forwarded_before_application_activation"]
async fn codex_proxy_front_helper() {
    let path = std::env::var_os("AGENTCHAT_TEST_CODEX_PROXY").unwrap();
    let target = std::env::var("AGENTCHAT_TEST_CODEX_TARGET").unwrap();
    let stream = UnixStream::connect(path).await.unwrap();
    let (mut socket, _) = client_async("ws://localhost/", stream).await.unwrap();
    socket
        .send(Message::Text(
            serde_json::json!({"id":7,"method":"thread/resume","params":{"threadId":target}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    if std::env::var_os("AGENTCHAT_TEST_CODEX_DOUBLE").is_some() {
        socket
            .send(Message::Text(
                serde_json::json!({"id":8,"method":"thread/resume","params":{"threadId":target}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        for _ in 0..2 {
            assert!(socket.next().await.unwrap().unwrap().is_text());
        }
        return;
    }
    let reply = socket.next().await;
    if std::env::var_os("AGENTCHAT_TEST_CODEX_UNKNOWN").is_some() {
        let value: Value =
            serde_json::from_str(reply.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(
            value["result"].is_object(),
            "native reply was suppressed by observation"
        );
        return;
    }
    let reply = reply.unwrap().unwrap().into_text().unwrap();
    if let Some(path) = std::env::var_os("AGENTCHAT_TEST_CODEX_SEEN") {
        let path = PathBuf::from(path);
        fs::write(&path, b"native received reply").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !path.with_extension("finish").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
    let reply = serde_json::from_str::<Value>(&reply).unwrap();
    if std::env::var_os("AGENTCHAT_TEST_CODEX_FAIL").is_some() {
        assert_eq!(reply["error"]["code"], -32600);
    } else {
        assert_eq!(reply["result"]["thread"]["id"], target);
    }
}

#[tokio::test]
async fn overlapping_transition_makes_the_original_candidate_unknown() {
    let home = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let backend_path = home.path().join("backend.sock");
    let backend = UnixListener::bind(&backend_path).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
    let initial = Uuid::new_v4().to_string();
    let proxy = Proxy::bind(home.path(), &backend_path, &initial).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert(
        OsString::from("AGENTCHAT_TEST_CODEX_PROXY"),
        proxy.path().as_os_str().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_CODEX_TARGET"),
        Uuid::new_v4().to_string().into(),
    );
    environment.insert(OsString::from("AGENTCHAT_TEST_CODEX_DOUBLE"), "1".into());
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from("codex::proxy::tests::codex_proxy_front_helper"),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(&executable, home.path(), &arguments, &environment)
        .await
        .unwrap();
    let mut events = proxy.serve(child.proof());
    let server = tokio::spawn(async move {
        let (stream, _) = backend.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        assert!(
            candidate_request(&socket.next().await.unwrap().unwrap())
                .unwrap()
                .is_some()
        );
        let second = socket.next().await.unwrap().unwrap();
        assert!(candidate_request(&second).unwrap().is_some());
        for id in [7, 8] {
            socket
                .send(Message::Text(
                    serde_json::json!({"id":id,"result":{"thread":{"id":"native-root"}}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        }
    });
    let (connection_epoch, transition_epoch, fence) = match events.recv().await.unwrap() {
        ProxyEvent::Candidate {
            connection_epoch,
            transition_epoch,
            fence,
        } => (connection_epoch, transition_epoch, fence),
        _ => panic!("expected held candidate"),
    };
    fence.send(()).unwrap();
    match events.recv().await.unwrap() {
        ProxyEvent::Candidate { fence, .. } => fence.send(()).unwrap(),
        _ => panic!("overlap must re-fence the exact current binding"),
    }
    match events.recv().await.unwrap() {
        ProxyEvent::Unknown {
            connection_epoch: connection,
            transition_epoch: transition,
        } => assert_eq!(
            (connection, transition),
            (connection_epoch, transition_epoch)
        ),
        _ => panic!("expected exact unknown candidate"),
    }
    assert!(child.wait().await.unwrap().success());
    server.await.unwrap();
}

#[tokio::test]
async fn native_success_is_forwarded_before_application_activation() {
    let home = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let backend_path = home.path().join("backend.sock");
    let backend = UnixListener::bind(&backend_path).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
    let initial = Uuid::new_v4().to_string();
    let target = Uuid::new_v4().to_string();
    let proxy = Proxy::bind(home.path(), &backend_path, &initial).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert(
        OsString::from("AGENTCHAT_TEST_CODEX_PROXY"),
        proxy.path().as_os_str().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_CODEX_TARGET"),
        target.clone().into(),
    );
    let seen = home.path().join("native-reply");
    environment.insert("AGENTCHAT_TEST_CODEX_SEEN".into(), seen.as_os_str().into());
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from("codex::proxy::tests::codex_proxy_front_helper"),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(&executable, home.path(), &arguments, &environment)
        .await
        .unwrap();
    let mut events = proxy.serve(child.proof());
    let server_target = target.clone();
    let (observed, mut observations) = mpsc::channel(1);
    let server = tokio::spawn(async move {
        let (stream, _) = backend.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let request = socket.next().await.unwrap().unwrap();
        assert!(candidate_request(&request).unwrap().is_some());
        observed.send(()).await.unwrap();
        socket
            .send(Message::Text(
                serde_json::json!({"id":7,"result":{"thread":{"id":server_target}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let fence = match events.recv().await.unwrap() {
        ProxyEvent::Candidate { fence, .. } => fence,
        _ => panic!("expected held candidate"),
    };
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), observations.recv())
            .await
            .is_err()
    );
    fence.send(()).unwrap();
    observations.recv().await.unwrap();
    let release = match events.recv().await.unwrap() {
        ProxyEvent::Succeeded { root, release, .. } => {
            assert_eq!(root, target);
            release
        }
        _ => panic!("expected held success"),
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !seen.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    release.send(ProxyDecision::Activated).unwrap();
    let release = match events.recv().await {
        Some(ProxyEvent::Delivered { release, .. }) => release,
        _ => panic!("expected held delivery release"),
    };
    release.send(()).unwrap();
    fs::write(seen.with_extension("finish"), b"finish").unwrap();
    assert!(child.wait().await.unwrap().success());
    server.await.unwrap();
}

#[tokio::test]
async fn resume_response_for_a_different_root_retains_the_exact_fence_as_unknown() {
    let home = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let backend_path = home.path().join("backend.sock");
    let backend = UnixListener::bind(&backend_path).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
    let initial = Uuid::new_v4().to_string();
    let requested = Uuid::new_v4().to_string();
    let unexpected = Uuid::new_v4().to_string();
    let proxy = Proxy::bind(home.path(), &backend_path, &initial).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert(
        OsString::from("AGENTCHAT_TEST_CODEX_PROXY"),
        proxy.path().as_os_str().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_CODEX_TARGET"),
        requested.clone().into(),
    );
    environment.insert(OsString::from("AGENTCHAT_TEST_CODEX_UNKNOWN"), "1".into());
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from("codex::proxy::tests::codex_proxy_front_helper"),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(&executable, home.path(), &arguments, &environment)
        .await
        .unwrap();
    let mut events = proxy.serve(child.proof());
    let server = tokio::spawn(async move {
        let (stream, _) = backend.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let (_, expected_root) = candidate_request(&socket.next().await.unwrap().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(expected_root.as_deref(), Some(requested.as_str()));
        socket
            .send(Message::Text(
                serde_json::json!({"id":7,"result":{"thread":{"id":unexpected}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let (connection_epoch, transition_epoch, fence) = match events.recv().await.unwrap() {
        ProxyEvent::Candidate {
            connection_epoch,
            transition_epoch,
            fence,
        } => (connection_epoch, transition_epoch, fence),
        _ => panic!("expected held candidate"),
    };
    fence.send(()).unwrap();
    match events.recv().await.unwrap() {
        ProxyEvent::Unknown {
            connection_epoch: connection,
            transition_epoch: transition,
        } => assert_eq!(
            (connection, transition),
            (connection_epoch, transition_epoch)
        ),
        _ => panic!("mismatched resume success must remain unknown"),
    }
    assert!(child.wait().await.unwrap().success());
    server.await.unwrap();
}

#[tokio::test]
async fn native_failure_is_forwarded_while_exact_fence_rolls_back() {
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
        OsString::from("AGENTCHAT_TEST_CODEX_PROXY"),
        proxy.path().as_os_str().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_CODEX_TARGET"),
        Uuid::new_v4().to_string().into(),
    );
    environment.insert(OsString::from("AGENTCHAT_TEST_CODEX_FAIL"), "1".into());
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from("codex::proxy::tests::codex_proxy_front_helper"),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(&executable, home.path(), &arguments, &environment)
        .await
        .unwrap();
    let mut events = proxy.serve(child.proof());
    let server = tokio::spawn(async move {
        let (stream, _) = backend.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        assert!(
            candidate_request(&socket.next().await.unwrap().unwrap())
                .unwrap()
                .is_some()
        );
        socket
            .send(Message::Text(
                serde_json::json!({"id":7,"error":{"code":-32600,"message":"cancelled"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let fence = match events.recv().await.unwrap() {
        ProxyEvent::Candidate { fence, .. } => fence,
        _ => panic!("expected held candidate"),
    };
    fence.send(()).unwrap();
    let release = match events.recv().await.unwrap() {
        ProxyEvent::RolledBack { release, .. } => release,
        _ => panic!("expected held rollback"),
    };
    release.send(()).unwrap();
    assert!(child.wait().await.unwrap().success());
    server.await.unwrap();
}

#[tokio::test]
async fn unrecognized_success_retains_the_exact_fence_as_unknown() {
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
        OsString::from("AGENTCHAT_TEST_CODEX_PROXY"),
        proxy.path().as_os_str().into(),
    );
    environment.insert(
        OsString::from("AGENTCHAT_TEST_CODEX_TARGET"),
        Uuid::new_v4().to_string().into(),
    );
    environment.insert(OsString::from("AGENTCHAT_TEST_CODEX_UNKNOWN"), "1".into());
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from("codex::proxy::tests::codex_proxy_front_helper"),
        OsString::from("--nocapture"),
    ];
    let mut child = NativeChild::spawn(&executable, home.path(), &arguments, &environment)
        .await
        .unwrap();
    let mut events = proxy.serve(child.proof());
    let server = tokio::spawn(async move {
        let (stream, _) = backend.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        assert!(
            candidate_request(&socket.next().await.unwrap().unwrap())
                .unwrap()
                .is_some()
        );
        socket
            .send(Message::Text(
                serde_json::json!({"id":7,"result":{"thread":{"displayName":"unknown"}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    });
    let (connection_epoch, transition_epoch, fence) = match events.recv().await.unwrap() {
        ProxyEvent::Candidate {
            connection_epoch,
            transition_epoch,
            fence,
        } => (connection_epoch, transition_epoch, fence),
        _ => panic!("expected held candidate"),
    };
    fence.send(()).unwrap();
    match events.recv().await.unwrap() {
        ProxyEvent::Unknown {
            connection_epoch: connection,
            transition_epoch: transition,
        } => assert_eq!(
            (connection, transition),
            (connection_epoch, transition_epoch)
        ),
        _ => panic!("expected indeterminate success to remain unknown"),
    }
    assert!(child.wait().await.unwrap().success());
    server.await.unwrap();
}
