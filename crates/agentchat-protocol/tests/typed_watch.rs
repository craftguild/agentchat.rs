//! Split-frame cancellation and explicit typed source errors preserve progress.
use agentchat_core::{
    Error, ErrorCode,
    model::Channel,
    read::{ChangeBatch, Checkpoint, ViewerChange, ViewerChangeKind},
    sequence::{Cursor, Sequence},
};
use agentchat_protocol::{
    framing,
    rpc::{Call, ChangeFrame, ChangeNotice, ChangeStart, Client, Id, Reply, Request, TypedReply},
};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixListener,
};
use uuid::Uuid;

fn key(request: &Request) -> Uuid {
    let Some(Id::Text(id)) = &request.id else {
        panic!("UUID correlation")
    };
    Uuid::parse_str(id).unwrap()
}
fn batch(after: Checkpoint) -> ChangeBatch {
    let seq = Sequence::new(1).unwrap();
    let event = Uuid::new_v4();
    let head = Checkpoint {
        project_id: after.project_id,
        seq: seq.cursor(),
        event_id: Some(event),
    };
    ChangeBatch {
        after,
        scan: head,
        head,
        has_more: false,
        items: vec![ViewerChange {
            event_id: event,
            seq,
            change: ViewerChangeKind::ChannelCreated {
                channel: Channel {
                    id: Uuid::new_v4(),
                    name: "日本語💜".into(),
                    creator_id: None,
                    creation_seq: seq,
                },
            },
        }],
    }
}
fn listener() -> (tempfile::TempDir, UnixListener, std::path::PathBuf) {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().join("typed-watch.sock");
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    (directory, listener, path)
}

#[tokio::test]
async fn cancellation_inside_unicode_frame_and_terminal_reset_never_advance_partial_progress() {
    let (_directory, listener, path) = listener();
    let project = Uuid::new_v4();
    let after = Checkpoint::start(project);
    let expected = batch(after);
    let body = expected.clone();
    let (prefix, seen) = tokio::sync::oneshot::channel();
    let (release, proceed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello: Request = framing::read(&mut read).await.unwrap().unwrap();
        framing::write(
            &mut write,
            &Reply::success(
                hello.id.clone(),
                key(&hello),
                project,
                serde_json::json!({"version":1,"project_id":project,"instance_id":Uuid::new_v4()}),
            ),
        )
        .await
        .unwrap();
        let request: Request = framing::read(&mut read).await.unwrap().unwrap();
        assert!(
            matches!(request.call().unwrap(),Call::SubscribeChanges {after:start} if start==after)
        );
        framing::write(
            &mut write,
            &TypedReply::success(
                request.id.clone(),
                key(&request),
                project,
                ChangeStart {
                    after,
                    head: body.head,
                },
            ),
        )
        .await
        .unwrap();
        let mut bytes = serde_json::to_vec(&ChangeFrame {
            jsonrpc: "2.0".into(),
            notice: ChangeNotice::Batch(body),
        })
        .unwrap();
        bytes.push(b'\n');
        let split = bytes.windows(3).position(|s| s == "日".as_bytes()).unwrap() + 1;
        write.write_all(&bytes[..split]).await.unwrap();
        prefix.send(()).unwrap();
        proceed.await.unwrap();
        write.write_all(&bytes[split..]).await.unwrap();
        let mut error = Error::new(ErrorCode::InvalidArgument, "History changed; reset 日本語.");
        error.details = serde_json::json!({"reset_required":true});
        framing::write(
            &mut write,
            &ChangeFrame {
                jsonrpc: "2.0".into(),
                notice: ChangeNotice::Error(error),
            },
        )
        .await
        .unwrap();
    });
    let mut follower = Client::connect(&path, project)
        .await
        .unwrap()
        .subscribe_changes(after)
        .await
        .unwrap();
    seen.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), follower.next())
            .await
            .is_err()
    );
    assert_eq!(follower.checkpoint(), after);
    release.send(()).unwrap();
    assert_eq!(follower.next().await.unwrap(), expected);
    let applied = follower.checkpoint();
    let error = follower.next().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["reset_required"], true);
    assert_eq!(follower.checkpoint(), applied);
    assert_eq!(
        follower.next().await.unwrap_err().code,
        ErrorCode::RuntimeUnavailable
    );
    server.await.unwrap();
}

#[tokio::test]
async fn malformed_typed_scan_range_poisoning_retains_the_last_complete_checkpoint() {
    let (_directory, listener, path) = listener();
    let project = Uuid::new_v4();
    let after = Checkpoint::start(project);
    let mut bad = batch(after);
    bad.scan.seq = Cursor::new(2).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello: Request = framing::read(&mut read).await.unwrap().unwrap();
        framing::write(
            &mut write,
            &Reply::success(
                hello.id.clone(),
                key(&hello),
                project,
                serde_json::json!({"version":1,"project_id":project,"instance_id":Uuid::new_v4()}),
            ),
        )
        .await
        .unwrap();
        let request: Request = framing::read(&mut read).await.unwrap().unwrap();
        framing::write(
            &mut write,
            &TypedReply::success(
                request.id.clone(),
                key(&request),
                project,
                ChangeStart {
                    after,
                    head: bad.head,
                },
            ),
        )
        .await
        .unwrap();
        framing::write(
            &mut write,
            &ChangeFrame {
                jsonrpc: "2.0".into(),
                notice: ChangeNotice::Batch(bad),
            },
        )
        .await
        .unwrap();
    });
    let mut follower = Client::connect(&path, project)
        .await
        .unwrap()
        .subscribe_changes(after)
        .await
        .unwrap();
    assert_eq!(
        follower.next().await.unwrap_err().code,
        ErrorCode::UnsupportedProtocol
    );
    assert_eq!(follower.checkpoint(), after);
    assert_eq!(
        follower.next().await.unwrap_err().code,
        ErrorCode::RuntimeUnavailable
    );
    server.await.unwrap();
}

#[tokio::test]
async fn a_continuation_cannot_replace_its_captured_history_head() {
    let (_directory, listener, path) = listener();
    let project = Uuid::new_v4();
    let after = Checkpoint::start(project);
    let mut first = batch(after);
    first.head = Checkpoint {
        project_id: project,
        seq: Cursor::new(2).unwrap(),
        event_id: Some(Uuid::new_v4()),
    };
    first.has_more = true;
    let complete = first.clone();
    let later = Checkpoint {
        project_id: project,
        seq: Cursor::new(3).unwrap(),
        event_id: Some(Uuid::new_v4()),
    };
    let second = ChangeBatch {
        after: first.scan,
        scan: later,
        head: later,
        items: Vec::new(),
        has_more: false,
    };
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello: Request = framing::read(&mut read).await.unwrap().unwrap();
        framing::write(
            &mut write,
            &Reply::success(
                hello.id.clone(),
                key(&hello),
                project,
                serde_json::json!({"version":1,"project_id":project,"instance_id":Uuid::new_v4()}),
            ),
        )
        .await
        .unwrap();
        let request: Request = framing::read(&mut read).await.unwrap().unwrap();
        framing::write(
            &mut write,
            &TypedReply::success(
                request.id.clone(),
                key(&request),
                project,
                ChangeStart {
                    after,
                    head: first.head,
                },
            ),
        )
        .await
        .unwrap();
        for body in [first, second] {
            framing::write(
                &mut write,
                &ChangeFrame {
                    jsonrpc: "2.0".into(),
                    notice: ChangeNotice::Batch(body),
                },
            )
            .await
            .unwrap();
        }
    });
    let mut follower = Client::connect(&path, project)
        .await
        .unwrap()
        .subscribe_changes(after)
        .await
        .unwrap();
    assert_eq!(follower.next().await.unwrap(), complete);
    assert_eq!(
        follower.next().await.unwrap_err().code,
        ErrorCode::UnsupportedProtocol
    );
    assert_eq!(follower.checkpoint(), complete.scan);
    server.await.unwrap();
}
