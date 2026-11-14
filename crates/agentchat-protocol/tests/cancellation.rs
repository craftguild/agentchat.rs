//! Partial frames survive cancellation; incomplete RPC exchanges require reconnect.

use agentchat_core::ErrorCode;
use agentchat_protocol::{
    framing::{self, FramedReader},
    rpc::{Client, Id, Reply, Request},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixListener,
};
use uuid::Uuid;

#[tokio::test]
async fn incremental_frames_keep_their_prefix_when_a_pending_read_is_cancelled() {
    let (read, mut write) = tokio::io::duplex(256);
    let mut read = FramedReader::new(BufReader::new(read));
    write.write_all(b"{\"prefix\":").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), read.read::<Value>())
            .await
            .is_err()
    );
    write.write_all(b"42}\n{\"next\":true}\n").await.unwrap();
    assert_eq!(
        read.read::<Value>().await.unwrap(),
        Some(json!({"prefix":42}))
    );
    assert_eq!(
        read.read::<Value>().await.unwrap(),
        Some(json!({"next":true}))
    );
    write
        .write_all(b"broken\n{\"valid\":true}\n")
        .await
        .unwrap();
    assert!(read.read::<Value>().await.is_err());
    assert!(
        read.read::<Value>().await.is_err(),
        "a failed frame must not silently resynchronize"
    );
}

#[tokio::test]
async fn cancelling_a_finite_call_prevents_reusing_its_connection_for_another_request() {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().join("rpc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let project = Uuid::new_v4();
    let (received, observed) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let request: Request = framing::read(&mut read).await.unwrap().unwrap();
        let key = match &request.id {
            Some(Id::Text(id)) => Uuid::parse_str(id).unwrap(),
            _ => panic!("client must correlate handshake"),
        };
        framing::write(
            &mut write,
            &Reply::success(
                request.id,
                key,
                project,
                json!({"version":1,"project_id":project,"instance_id":Uuid::new_v4()}),
            ),
        )
        .await
        .unwrap();
        let _: Request = framing::read(&mut read).await.unwrap().unwrap();
        received.send(()).unwrap();
        assert!(
            framing::read::<_, Request>(&mut read)
                .await
                .unwrap()
                .is_none(),
            "poisoned clients must not send a second operation"
        );
    });
    let mut client = Client::connect(&path, project).await.unwrap();
    let mut pending = Box::pin(client.health());
    tokio::select! {_=observed=>{},result=&mut pending=>panic!("fixture must hold the response: {result:?}")}
    drop(pending);
    assert_eq!(
        client.health().await.unwrap_err().code,
        ErrorCode::RuntimeUnavailable
    );
    drop(client);
    task.await.unwrap();
}
