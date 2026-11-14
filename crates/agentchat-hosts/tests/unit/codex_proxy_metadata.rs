use super::*;
use tokio_tungstenite::tungstenite::Message;

fn text(value: impl Into<String>) -> Message {
    Message::Text(value.into().into())
}

#[test]
fn large_unrelated_trees_and_escaped_metadata_are_skipped() {
    let body = r#"{"input":[{"text":"private\\\"\n\u0000"}],"tool":{"output":[1,true,null]}}"#;
    let payload = std::iter::repeat_n(body, 32768)
        .collect::<Vec<_>>()
        .join(",");
    let key = serde_json::to_string("request\n\"").unwrap();
    let request = text(format!(
        r#"{{"params":{{"history":[{payload}],"thread\u0049d":"root-\u0062"}},"ignored":[{payload}],"method":"thread\/resume","id":"r\u0065quest\n\""}}"#
    ));
    assert_eq!(
        candidate_request(&request).unwrap(),
        Some((key.clone(), Some("root-b".into())))
    );
    let reply = text(format!(
        r#"{{"result":{{"turns":[{payload}],"thread":{{"body":[{payload}],"id":"root-b"}}}},"id":"request\n\""}}"#
    ));
    assert_eq!(
        candidate_response(&reply, |id| id == key).unwrap(),
        Some((key.clone(), CandidateOutcome::Succeeded("root-b".into())))
    );
    assert!(candidate_response(&reply, |_| false).unwrap().is_none());
    assert_eq!(
        metadata::envelope(&request).unwrap().method().as_deref(),
        Some("thread/resume")
    );
}

#[test]
fn only_scalar_bounded_ids_can_enter_lifecycle_correlation() {
    for id in [r#"{"input":"private"}"#, "[1,2]", "true"] {
        assert!(
            candidate_request(&text(format!(r#"{{"id":{id},"method":"thread/start"}}"#))).is_err()
        );
        assert!(
            metadata::envelope(&text(format!(r#"{{"id":{id}}}"#)))
                .unwrap()
                .request_id()
                .is_none()
        );
    }
    for id in [
        "7",
        "-1",
        "1.25e+10",
        "1E-04",
        "1.0",
        "0e+00",
        "12345678901234567890123456789012345678901234567890",
        "-0",
        "null",
        r#""escaped\u002did""#,
    ] {
        let request = text(format!(r#"{{"id":{id},"method":"thread/start"}}"#));
        let (key, _) = candidate_request(&request).unwrap().unwrap();
        let previous: serde_json::Value = serde_json::from_str(id).unwrap();
        assert_eq!(key, serde_json::to_string(&previous).unwrap());
        let reply = text(format!(r#"{{"id":{id},"result":{{"threadId":"root"}}}}"#));
        assert_eq!(
            candidate_response(&reply, |id| id == key)
                .unwrap()
                .unwrap()
                .1,
            CandidateOutcome::Succeeded("root".into())
        );
    }
    for length in [8190, 8191] {
        let request = text(format!(
            r#"{{"id":"{}","method":"thread/start"}}"#,
            "x".repeat(length)
        ));
        assert_eq!(candidate_request(&request).is_ok(), length == 8190);
    }
    let encoded_root = "\\u0061".repeat(1024);
    let request = text(format!(
        r#"{{"id":1,"method":"thread/resume","params":{{"threadId":"{encoded_root}"}}}}"#
    ));
    assert_eq!(
        candidate_request(&request).unwrap().unwrap().1.unwrap(),
        "a".repeat(1024)
    );
}

#[test]
fn duplicate_keys_null_and_changed_shapes_keep_conservative_outcomes() {
    let request = text(
        r#"{"id":1,"id":"last","method":"thread/start","method":"thread/resume","params":{"threadId":"old","threadId":"new"}}"#,
    );
    assert_eq!(
        candidate_request(&request).unwrap(),
        Some((r#""last""#.into(), Some("new".into())))
    );
    let internal =
        text(r#"{"id":1,"method":"thread/start","params":{"ephemeral":false,"ephemeral":true}}"#);
    assert!(candidate_request(&internal).unwrap().is_none());
    for (result, expected) in [
        (
            r#"{"thread":{"id":null},"threadId":"fallback"}"#,
            CandidateOutcome::Indeterminate,
        ),
        (
            r#"{"thread":{"id":false},"threadId":"fallback"}"#,
            CandidateOutcome::Indeterminate,
        ),
        (
            r#"{"thread":{"displayName":"changed"},"threadId":"fallback"}"#,
            CandidateOutcome::Succeeded("fallback".into()),
        ),
        (
            r#"{"thread":[{"id":"wrong"}]}"#,
            CandidateOutcome::Indeterminate,
        ),
        (
            r#"{"thread":{"id":"root\nother"}}"#,
            CandidateOutcome::Indeterminate,
        ),
        (
            r#"{"thread":{"id":"old","id":"last"}}"#,
            CandidateOutcome::Succeeded("last".into()),
        ),
    ] {
        let reply = text(format!(r#"{{"id":1,"error":null,"result":{result}}}"#));
        assert_eq!(
            candidate_response(&reply, |id| id == "1")
                .unwrap()
                .unwrap()
                .1,
            expected
        );
    }
    let changed = text(
        r#"{"id":"old","id":1,"error":{},"error":null,"result":{"threadId":"old"},"result":{"threadId":"last"}}"#,
    );
    assert_eq!(
        candidate_response(&changed, |id| id == "1")
            .unwrap()
            .unwrap()
            .1,
        CandidateOutcome::Succeeded("last".into())
    );
    let changed =
        text(r#"{"id":1,"method":"thread/resume","params":{"threadId":"old"},"params":null}"#);
    assert!(candidate_request(&changed).is_err());
    let failure = text(r#"{"id":1,"error":false,"result":{"threadId":"wrong"}}"#);
    assert_eq!(
        candidate_response(&failure, |_| true).unwrap().unwrap().1,
        CandidateOutcome::Failed
    );
    for malformed in ["not-json", "[]", r#"{"id":1,"result":{},"broken":}"#] {
        assert!(candidate_request(&text(malformed)).unwrap().is_none());
        assert!(
            candidate_response(&text(malformed), |_| true)
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn forwarding_preserves_large_frames_and_correlates_after_native_delivery() {
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio_tungstenite::{accept_async, client_async};
    let home = tempfile::tempdir().unwrap();
    let backend = home.path().join("backend.sock");
    let listener = UnixListener::bind(&backend).unwrap();
    let (front, client) = UnixStream::pair().unwrap();
    let (sender, mut events) = mpsc::channel(8);
    let (observer, observing) = observation::unbound("root-a", sender);
    let relay_observer = observer.clone();
    let proxy =
        tokio::spawn(async move { bridge::bridge(front, &backend, 1, &relay_observer).await });
    let client_task =
        tokio::spawn(async move { client_async("ws://localhost/", client).await.unwrap().0 });
    let (raw, _) = listener.accept().await.unwrap();
    let mut server = accept_async(raw).await.unwrap();
    let mut client = client_task.await.unwrap();
    let body = serde_json::to_string(&"private\\\"\n".repeat(65536)).unwrap();
    let request = text(format!(
        r#"{{ "params":{{"threadId":"root-b","history":{body}}}, "id":"\u0061", "method":"thread/resume" }}"#
    ));
    let unrelated = text(format!(
        r#"{{ "id":"unrelated", "result":{{"thread":{{"id":"wrong","body":{body}}}}} }}"#
    ));
    let response = text(format!(
        r#"{{ "id":"a", "result":{{"thread":{{"id":"root-b","history":{body}}}}} }}"#
    ));
    let expected_request = request.clone();
    let native_unrelated = unrelated.clone();
    let native_response = response.clone();
    // Real endpoints make progress independently. Waiting for a complete large
    // send before polling its receiver would deadlock any bounded byte relay.
    let backend = tokio::spawn(async move {
        assert_eq!(server.next().await.unwrap().unwrap(), expected_request);
        server.send(unrelated).await.unwrap();
        server.send(response).await.unwrap();
    });
    let (received, delivered) = tokio::sync::oneshot::channel();
    let native = tokio::spawn(async move {
        client.send(request).await.unwrap();
        assert_eq!(client.next().await.unwrap().unwrap(), native_unrelated);
        assert_eq!(client.next().await.unwrap().unwrap(), native_response);
        received.send(()).unwrap();
    });
    match events.recv().await.unwrap() {
        ProxyEvent::Candidate { fence, .. } => fence.send(()).unwrap(),
        _ => panic!("expected fenced candidate"),
    }
    let release = match events.recv().await.unwrap() {
        ProxyEvent::Succeeded { root, release, .. } => {
            assert_eq!(root, "root-b");
            release
        }
        _ => panic!("only a matching delivered native success may activate"),
    };
    delivered.await.unwrap();
    release.send(ProxyDecision::Activated).unwrap();
    match events.recv().await.unwrap() {
        ProxyEvent::Delivered { release, .. } => release.send(()).unwrap(),
        _ => panic!("expected delivery gate"),
    }
    native.await.unwrap();
    backend.await.unwrap();
    proxy.abort();
    observing.abort();
    let _ = proxy.await;
}
