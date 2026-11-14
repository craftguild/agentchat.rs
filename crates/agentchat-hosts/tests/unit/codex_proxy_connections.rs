use super::*;
use crate::launch::NativeChild;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::BTreeMap, ffi::OsString, time::Duration};
use tokio_tungstenite::{WebSocketStream, accept_async, client_async, tungstenite::Message};
type Socket = WebSocketStream<UnixStream>;
const WAIT: Duration = Duration::from_secs(10);
async fn connect(path: &Path) -> Socket {
    tokio::time::timeout(WAIT, async {
        client_async(
            "ws://localhost/rpc",
            UnixStream::connect(path).await.unwrap(),
        )
        .await
        .unwrap()
        .0
    })
    .await
    .unwrap()
}
async fn rpc(s: &mut Socket, id: u64, method: &str, params: Value) -> Value {
    s.send(Message::Text(
        json!({"id":id,"method":method,"params":params})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let r = tokio::time::timeout(WAIT, s.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(r.to_text().unwrap()).unwrap()
}
#[tokio::test]
#[ignore = "exact-owner frontend invoked by all_connections_forward_and_correlate_by_connection"]
async fn codex_proxy_connections_helper() {
    let path = PathBuf::from(std::env::var_os("AGENTCHAT_TEST_MULTI_PROXY").unwrap());
    let target = std::env::var("AGENTCHAT_TEST_MULTI_TARGET").unwrap();
    let mut sockets = Vec::new();
    for i in 0..8 {
        let mut s = connect(&path).await;
        assert_eq!(
            rpc(&mut s, 1, "initialize", json!({"index":i})).await["result"]["ok"],
            true
        );
        sockets.push(s);
    }
    for (i, method) in [
        "thread/items/list",
        "thread/turns/list",
        "thread/archive",
        "thread/name/set",
        "future/native/api",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            rpc(&mut sockets[i + 1], 2, method, json!({})).await["result"]["method"],
            method
        );
    }
    // Server-initiated request replies have no method and must pass unchanged.
    sockets[2]
        .send(Message::Text(
            json!({"id":33,"result":{"answers":{}}}).to_string().into(),
        ))
        .await
        .unwrap();
    let ack = sockets[2]
        .next()
        .await
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&ack).unwrap()["result"]["method"],
        Value::Null
    );
    sockets[7]
        .send(Message::Text(
            json!({"id":7,"method":"thread/resume","params":{"threadId":target}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    // The same ID on another socket cannot satisfy the resume correlation.
    assert_eq!(
        rpc(&mut sockets[0], 7, "test/unrelated", json!({})).await["result"]["thread"]["id"],
        "unrelated-root"
    );
    let reply = sockets[7]
        .next()
        .await
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&reply).unwrap()["result"]["thread"]["id"],
        target
    );
    sockets.remove(0);
    assert_eq!(
        rpc(&mut sockets[0], 9, "thread/items/list", json!({})).await["result"]["ok"],
        true
    );
    assert_eq!(
        rpc(&mut sockets[0], 10, "test/backend-unlink", json!({})).await["result"]["ok"],
        true
    );
    let raw = UnixStream::connect(&path).await.unwrap();
    assert!(
        tokio::time::timeout(WAIT, client_async("ws://localhost/", raw))
            .await
            .unwrap()
            .is_err()
    );
    // Failure of that new connection cannot end any established sibling.
    assert_eq!(
        rpc(&mut sockets[0], 11, "thread/items/list", json!({})).await["result"]["ok"],
        true
    );
    assert_eq!(
        rpc(&mut sockets[0], 12, "test/backend-restore", json!({})).await["result"]["ok"],
        true
    );
    let mut restored = connect(&path).await;
    assert_eq!(
        rpc(&mut restored, 13, "thread/items/list", json!({})).await["result"]["ok"],
        true
    );
}
#[tokio::test]
async fn all_connections_forward_and_correlate_by_connection() {
    let home = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let path = home.path().join("backend.sock");
    let mut backend = UnixListener::bind(&path).unwrap();
    let proxy = Proxy::bind(home.path(), &path, &Uuid::new_v4().to_string()).unwrap();
    let proxy_path = proxy.path().to_owned();
    let target = Uuid::new_v4().to_string();
    let environment = BTreeMap::from([
        (
            OsString::from("AGENTCHAT_TEST_MULTI_PROXY"),
            proxy.path().as_os_str().into(),
        ),
        (
            OsString::from("AGENTCHAT_TEST_MULTI_TARGET"),
            target.clone().into(),
        ),
    ]);
    let args = [
        "--ignored",
        "--exact",
        "codex::proxy::connection_tests::codex_proxy_connections_helper",
        "--nocapture",
    ]
    .map(OsString::from);
    let mut child = NativeChild::spawn(
        &std::env::current_exe().unwrap(),
        home.path(),
        &args,
        &environment,
    )
    .await
    .unwrap();
    let mut events = proxy.serve(child.proof());
    let (release, released) = tokio::sync::watch::channel(false);
    let (replace, mut replacements) = tokio::sync::mpsc::channel(1);
    let server_target = target.clone();
    let server = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            let raw = tokio::select! {
                accepted = backend.accept() => accepted.unwrap().0,
                replacement = replacements.recv() => {
                    backend = replacement.unwrap();
                    continue;
                }
            };
            let mut released = released.clone();
            let target = server_target.clone();
            let release = release.clone();
            let path = path.clone();
            let replace = replace.clone();
            tasks.spawn(async move {
                let mut s = accept_async(raw).await.unwrap();
                while let Some(Ok(Message::Text(raw))) = s.next().await {
                    let v: Value = serde_json::from_str(&raw).unwrap();
                    let mut result = json!({"ok":true,"method":v["method"]});
                    if v["method"] == "thread/resume" {
                        while !*released.borrow_and_update() {
                            released.changed().await.unwrap();
                        }
                        result = json!({"thread":{"id":target}});
                    } else if v["method"] == "test/unrelated" {
                        result = json!({"thread":{"id":"unrelated-root"}});
                        release.send(true).unwrap();
                    } else if v["method"] == "test/backend-unlink" {
                        fs::remove_file(&path).unwrap();
                    } else if v["method"] == "test/backend-restore" {
                        replace
                            .send(UnixListener::bind(&path).unwrap())
                            .await
                            .unwrap();
                    }
                    if s.send(Message::Text(
                        json!({"id":v["id"],"result":result}).to_string().into(),
                    ))
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
    match tokio::time::timeout(WAIT, events.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ProxyEvent::Candidate {
            connection_epoch: 8,
            transition_epoch: 1,
            fence,
        } => {
            // A same-user parent is still not the designated native child.
            let raw = UnixStream::connect(&proxy_path).await.unwrap();
            assert!(
                tokio::time::timeout(WAIT, client_async("ws://localhost/", raw))
                    .await
                    .unwrap()
                    .is_err()
            );
            fence.send(()).unwrap();
        }
        _ => panic!("only the actual request, on socket eight, is a candidate"),
    }
    match events.recv().await.unwrap() {
        ProxyEvent::Succeeded {
            connection_epoch: 8,
            transition_epoch: 1,
            root,
            release,
        } => {
            assert_eq!(root, target);
            release.send(ProxyDecision::Activated).unwrap();
        }
        _ => panic!("unrelated same-ID reply acquired transition authority"),
    }
    match events.recv().await.unwrap() {
        ProxyEvent::Delivered {
            connection_epoch: 8,
            transition_epoch: 1,
            release,
        } => release.send(()).unwrap(),
        _ => panic!("missing correlated delivery"),
    }
    assert!(
        tokio::time::timeout(WAIT, child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    server.abort();
    let _ = server.await;
}
