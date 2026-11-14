use super::*;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio_tungstenite::{
    WebSocketStream, accept_async_with_config, client_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
type Socket = WebSocketStream<UnixStream>;
const WAIT: Duration = Duration::from_secs(20);
fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None)
}
struct Tasks(Vec<tokio::task::JoinHandle<()>>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}
fn observation_tasks() -> (observation::Observer, Tasks) {
    let (sender, mut events) = mpsc::channel(16);
    let (observer, task) = observation::unbound("root-a", sender);
    let controls = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ProxyEvent::Candidate { fence, .. } => {
                    let _ = fence.send(());
                }
                ProxyEvent::Succeeded { release, .. } => {
                    let _ = release.send(ProxyDecision::Activated);
                }
                ProxyEvent::RolledBack { release, .. } | ProxyEvent::Delivered { release, .. } => {
                    let _ = release.send(());
                }
                ProxyEvent::Unknown { .. } => {}
            }
        }
    });
    (observer, Tasks(vec![task, controls]))
}
async fn sockets() -> (Socket, Socket, Tasks, tempfile::TempDir) {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let path = home.path().join("backend.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (front, raw_client) = UnixStream::pair().unwrap();
    let (observer, mut tasks) = observation_tasks();
    tasks.0.push(tokio::spawn(async move {
        let _ = bridge::bridge(front, &path, 1, &observer).await;
    }));
    let client = tokio::spawn(async move {
        client_async_with_config("ws://localhost/rpc", raw_client, Some(config()))
            .await
            .unwrap()
            .0
    });
    let (raw, _) = listener.accept().await.unwrap();
    let server = accept_async_with_config(raw, Some(config())).await.unwrap();
    (client.await.unwrap(), server, tasks, home)
}
async fn peek(socket: &Socket) {
    tokio::time::timeout(WAIT, async {
        loop {
            let mut byte = [0u8; 1];
            match rustix::net::recv(
                socket.get_ref(),
                &mut byte[..],
                rustix::net::RecvFlags::PEEK | rustix::net::RecvFlags::DONTWAIT,
            ) {
                Ok((n, _)) if n > 0 => break,
                Err(rustix::io::Errno::AGAIN) => {}
                other => panic!("peek: {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn frames_above_the_old_64_mib_limit_are_forwarded_both_ways() {
    use sha2::{Digest, Sha256};
    let (mut client, mut server, _tasks, _home) = sockets().await;
    let text = format!(
        r#"{{"id":1,"method":"thread/items/list","params":{{"synthetic":"{}"}}}}"#,
        "x".repeat((64 << 20) + 1)
    );
    let length = text.len();
    let digest = Sha256::digest(text.as_bytes());
    let backend = tokio::spawn(async move {
        let frame = server.next().await.unwrap().unwrap();
        assert_eq!(frame.len(), length);
        server.send(frame).await.unwrap();
    });
    client.send(Message::Text(text.into())).await.unwrap();
    let reply = tokio::time::timeout(WAIT, client.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply.len(), length);
    assert_eq!(Sha256::digest(reply.into_data()), digest);
    backend.await.unwrap();
}
#[tokio::test]
async fn a_blocked_backend_write_does_not_block_reverse_replies() {
    let (client, mut server, _tasks, _home) = sockets().await;
    let (mut write, mut read) = client.split();
    let body = format!(
        r#"{{"method":"turn/start","params":{{"synthetic":"{}"}}}}"#,
        "x".repeat(8 << 20)
    );
    let sending = tokio::spawn(async move {
        write.send(Message::Text(body.into())).await.unwrap();
        write
    });
    peek(&server).await; // The upstream write has entered a backend that is not draining it.
    server
        .send(Message::Text("reverse-progress".into()))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(WAIT, read.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
        "reverse-progress"
    );
    assert!(server.next().await.unwrap().unwrap().len() > 8 << 20);
    let _write = sending.await.unwrap();
}
#[tokio::test]
async fn a_blocked_frontend_write_does_not_block_forward_requests() {
    let (mut client, server, _tasks, _home) = sockets().await;
    let (mut write, mut read) = server.split();
    let sending = tokio::spawn(async move {
        write
            .send(Message::Text("x".repeat(8 << 20).into()))
            .await
            .unwrap();
        write
    });
    peek(&client).await;
    client
        .send(Message::Text(
            r#"{"method":"thread/turns/list","id":9}"#.into(),
        ))
        .await
        .unwrap();
    let request = tokio::time::timeout(WAIT, read.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(request.to_text().unwrap().contains("thread/turns/list"));
    assert_eq!(client.next().await.unwrap().unwrap().len(), 8 << 20);
    let _write = sending.await.unwrap();
}
#[tokio::test]
async fn binary_rpc_replies_and_control_frames_remain_native() {
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
    let (mut client, mut server, _tasks, _home) = sockets().await;
    client
        .send(Message::Binary(vec![0, 255, 27, 128].into()))
        .await
        .unwrap();
    assert_eq!(
        server.next().await.unwrap().unwrap(),
        Message::Binary(vec![0, 255, 27, 128].into())
    );
    client
        .send(Message::Text(
            r#"{"id":17,"result":{"answer":"native"}}"#.into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        server.next().await.unwrap().unwrap().to_text().unwrap(),
        r#"{"id":17,"result":{"answer":"native"}}"#
    );
    client
        .send(Message::Ping(vec![9, 8, 7].into()))
        .await
        .unwrap();
    // The relay must not invent a Pong before the real endpoint reads Ping.
    assert!(
        tokio::time::timeout(Duration::from_millis(25), client.next())
            .await
            .is_err()
    );
    assert_eq!(
        server.next().await.unwrap().unwrap(),
        Message::Ping(vec![9, 8, 7].into())
    );
    server.flush().await.unwrap();
    assert_eq!(
        tokio::time::timeout(WAIT, client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Message::Pong(vec![9, 8, 7].into())
    );
    client
        .send(Message::Close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "native exit".into(),
        })))
        .await
        .unwrap();
    loop {
        match tokio::time::timeout(WAIT, server.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Close(Some(frame)) => {
                assert_eq!(frame.reason, "native exit");
                break;
            }
            Message::Pong(_) => {}
            other => panic!("unexpected close traffic {other:?}"),
        }
    }
    server.flush().await.unwrap();
    assert!(matches!(
        tokio::time::timeout(WAIT, client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Message::Close(_)
    ));
}
#[tokio::test]
async fn native_upgrade_uri_repeated_headers_and_selected_protocol_are_preserved() {
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::{
            client::IntoClientRequest,
            handshake::server::{Request, Response},
        },
    };
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let path = home.path().join("backend.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (front, raw) = UnixStream::pair().unwrap();
    let (observer, mut tasks) = observation_tasks();
    tasks.0.push(tokio::spawn(async move {
        let _ = bridge::bridge(front, &path, 1, &observer).await;
    }));
    let mut request = "ws://localhost/rpc?fixture=owned"
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .append("x-native-header", "one".parse().unwrap());
    request
        .headers_mut()
        .append("x-native-header", "two".parse().unwrap());
    request
        .headers_mut()
        .insert("sec-websocket-protocol", "native-test".parse().unwrap());
    let client = tokio::spawn(async move {
        client_async_with_config(request, raw, Some(config()))
            .await
            .unwrap()
    });
    let (raw, _) = listener.accept().await.unwrap();
    let _server = accept_hdr_async(
        raw,
        #[allow(clippy::result_large_err)] // Tungstenite fixes the callback error type.
        |r: &Request, mut response: Response| {
            assert_eq!(r.uri(), "/rpc?fixture=owned");
            assert_eq!(r.headers().get_all("x-native-header").iter().count(), 2);
            response
                .headers_mut()
                .insert("sec-websocket-protocol", "native-test".parse().unwrap());
            response
                .headers_mut()
                .insert("x-native-response", "preserved".parse().unwrap());
            Ok(response)
        },
    )
    .await
    .unwrap();
    let (_client, response) = client.await.unwrap();
    assert_eq!(response.headers()["sec-websocket-protocol"], "native-test");
    assert_eq!(response.headers()["x-native-response"], "preserved");
}
#[tokio::test]
async fn failed_backend_upgrade_is_returned_without_a_false_success() {
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::{
            Error,
            handshake::server::{Request, Response},
        },
    };
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let path = home.path().join("backend.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (front, raw) = UnixStream::pair().unwrap();
    let (observer, mut tasks) = observation_tasks();
    tasks.0.push(tokio::spawn(async move {
        let _ = bridge::bridge(front, &path, 1, &observer).await;
    }));
    let client = tokio::spawn(async move {
        client_async_with_config("ws://localhost/rpc", raw, Some(config())).await
    });
    let (raw, _) = listener.accept().await.unwrap();
    let server = accept_hdr_async(
        raw,
        #[allow(clippy::result_large_err)] // Tungstenite fixes the callback error type.
        |_: &Request, _: Response| {
            Err(tokio_tungstenite::tungstenite::http::Response::builder()
                .status(403)
                .header("x-native-error", "exact")
                .body(Some("native denied".to_owned()))
                .unwrap())
        },
    )
    .await;
    assert!(server.is_err());
    match client.await.unwrap() {
        Err(Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 403);
            assert_eq!(response.headers()["x-native-error"], "exact");
        }
        other => panic!("unexpected upgrade outcome {other:?}"),
    }
}

#[tokio::test]
async fn fragmented_text_with_interleaved_native_ping_preserves_lifecycle_metadata() {
    use tokio_tungstenite::tungstenite::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    let (mut client, mut server, _tasks, _home) = sockets().await;
    let request=br#"{"params":{"history":"ignored","threadId":"root-b"},"id":"fragment","method":"thread/resume"}"#;
    client
        .send(Message::Frame(Frame::message(
            request[..17].to_vec(),
            OpCode::Data(Data::Text),
            false,
        )))
        .await
        .unwrap();
    client.send(Message::Ping(vec![4, 5].into())).await.unwrap();
    client
        .send(Message::Frame(Frame::message(
            request[17..].to_vec(),
            OpCode::Data(Data::Continue),
            true,
        )))
        .await
        .unwrap();
    assert_eq!(
        server.next().await.unwrap().unwrap(),
        Message::Ping(vec![4, 5].into())
    );
    assert_eq!(
        server.next().await.unwrap().unwrap().into_data(),
        request.as_slice()
    );
    server.flush().await.unwrap();
    assert_eq!(
        client.next().await.unwrap().unwrap(),
        Message::Pong(vec![4, 5].into())
    );
    let response = br#"{"id":"fragment","result":{"thread":{"id":"root-b"}}}"#;
    server
        .send(Message::Frame(Frame::message(
            response[..11].to_vec(),
            OpCode::Data(Data::Text),
            false,
        )))
        .await
        .unwrap();
    server
        .send(Message::Frame(Frame::message(
            response[11..].to_vec(),
            OpCode::Data(Data::Continue),
            true,
        )))
        .await
        .unwrap();
    assert_eq!(
        client.next().await.unwrap().unwrap().into_data(),
        response.as_slice()
    );
}

#[tokio::test]
async fn streamed_metadata_preserves_duplicates_escapes_and_numeric_id_spelling() {
    for raw in [
        r#"{"params":{"unused":[{"prompt":"not retained"}],"threadId":"root-b"},"id":1E-04,"method":"thread\/resume"}"#,
        r#"{"id":1,"id":"last","method":"thread/start","method":"thread/resume","params":{"threadId":"old","threadId":"root-b"}}"#,
        r#"{"id":1,"method":"thread/start","params":{"ephemeral":false,"ephemeral":true}}"#,
        r#"{"id":"reply","result":{"thread":{"id":null},"threadId":"fallback"}}"#,
        r#"{"id":"reply","error":{},"error":null,"result":{"thread":{"id":"old","id":"root-b"}}}"#,
        r#"{"id":"reply","result":{"thread":false,"threadId":"fallback"}}"#,
        r#"{"id":"reply","result":{"thread":[{"body":"not retained"}],"threadId":"fallback"}}"#,
        r#"{"id":"reply","result":{"thread":"not retained","threadId":"fallback"}}"#,
        r#"{"id":"reply","result":{"thread":{"id":"old"},"thread":42,"threadId":"fallback"}}"#,
        r#"{"id":"reply","result":{"threadId":"old"},"result":false}"#,
        r#"{"id":1,"method":"thread/resume","params":{"threadId":"old"},"params":["not retained"]}"#,
    ] {
        let expected = Message::Text(raw.into());
        let mut feed = json_stream::Feed::new();
        for chunk in raw.as_bytes().chunks(3) {
            for b in chunk {
                feed.push(*b);
            }
            assert!(feed.flush().await);
        }
        let parsed = feed
            .finish()
            .await
            .expect("bounded metadata remains observable");
        assert!(parsed.len() < 256);
        assert!(!parsed.to_text().unwrap().contains("not retained"));
        assert_eq!(
            metadata::candidate_request(&parsed),
            metadata::candidate_request(&expected)
        );
        assert_eq!(
            metadata::candidate_response(&parsed, |_| true),
            metadata::candidate_response(&expected, |_| true)
        );
    }
}

#[tokio::test]
async fn unobservable_metadata_does_not_reject_or_rewrite_native_requests() {
    let (mut client, mut server, _tasks, _home) = sockets().await;
    for raw in [
        r#"{"id":{"synthetic":"compound"},"method":"thread/resume","params":{"threadId":"root-b"}}"#.to_owned(),
        format!(r#"{{"id":"{}","method":"thread/resume","params":{{"threadId":"root-b"}}}}"#,"x".repeat(9000)),
        r#"{"id":9,"method":"future/native/method","params":{"payload":"after unknown"}}"#.to_owned(),
    ] {
        client.send(Message::Text(raw.clone().into())).await.unwrap();
        assert_eq!(tokio::time::timeout(WAIT,server.next()).await.unwrap().unwrap().unwrap().to_text().unwrap(),raw);
    }
}
#[tokio::test]
async fn text_observation_validates_utf8_across_fragment_boundaries() {
    for bytes in [
        "日本語🙂".as_bytes(),
        &[0xc0, 0x80],
        &[0xed, 0xa0, 0x80],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0xf0, 0x9f],
    ] {
        let mut validation = utf8::Utf8::default();
        let mut valid = true;
        for byte in bytes {
            valid &= validation.push(*byte);
        }
        assert_eq!(
            valid && validation.complete(),
            std::str::from_utf8(bytes).is_ok()
        );
    }
}

#[tokio::test]
async fn excessive_observation_depth_does_not_limit_native_json() {
    let raw = format!(
        r#"{{"id":1,"method":"future/native","params":{{"unused":{}0{}}}}}"#,
        "[".repeat(256),
        "]".repeat(256)
    );
    let mut feed = json_stream::Feed::new();
    for byte in raw.bytes() {
        feed.push(byte);
    }
    assert!(feed.finish().await.is_none());
    let (mut client, mut server, _tasks, _home) = sockets().await;
    client
        .send(Message::Text(raw.clone().into()))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(WAIT, server.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
        raw
    );
}

#[tokio::test]
async fn reserved_data_opcode_cannot_complete_a_lifecycle_observation() {
    let json = br#"{"id":1,"method":"thread/resume","params":{"threadId":"root-b"}}"#;
    let mut wire = b"GET /rpc HTTP/1.1\r\n\r\n".to_vec();
    for (opcode, body) in [(1u8, &json[..17]), (0x83u8, &json[17..])] {
        let mask = [1u8, 2, 3, 4];
        wire.extend_from_slice(&[opcode, 0x80 | body.len() as u8]);
        wire.extend_from_slice(&mask);
        wire.extend(body.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    }
    let mut tap = frame_tap::Tap::new(true);
    let mut packets = 0;
    for b in wire {
        if tap.byte(b, true) {
            assert!(matches!(
                tap.finish().await,
                frame_tap::Packet::Unobservable
            ));
            packets += 1;
        }
    }
    assert_eq!(packets, 1);
}

#[tokio::test]
async fn native_half_close_preserves_the_opposite_direction_until_eof() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let path = home.path().join("backend.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (front, mut client) = UnixStream::pair().unwrap();
    let (observer, _tasks) = observation_tasks();
    let relay = tokio::spawn(async move { bridge::bridge(front, &path, 1, &observer).await });
    let (mut backend, _) = listener.accept().await.unwrap();
    let request = b"GET /native HTTP/1.1\r\nHost: localhost\r\n\r\n";
    client.write_all(request).await.unwrap();
    client.shutdown().await.unwrap();
    let mut received = Vec::new();
    tokio::time::timeout(WAIT, backend.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, request);
    let response = b"HTTP/1.1 400 Native failure\r\nContent-Length: 4\r\n\r\nbody";
    backend.write_all(response).await.unwrap();
    backend.shutdown().await.unwrap();
    received.clear();
    tokio::time::timeout(WAIT, client.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, response);
    assert!(relay.await.unwrap().is_ok());
}
