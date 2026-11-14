use super::*;
use crate::launch::NativeChild;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::{collections::BTreeMap, ffi::OsString, time::Duration};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const WAIT: Duration = Duration::from_secs(10);

struct Startup {
    home: tempfile::TempDir,
    child: NativeChild,
    events: ProxyEvents,
    loaded: oneshot::Receiver<()>,
    server: tokio::task::JoinHandle<()>,
    seen: PathBuf,
}

async fn startup(failed: bool) -> Startup {
    let home = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let backend_path = home.path().join("backend.sock");
    let backend = UnixListener::bind(&backend_path).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o600)).unwrap();
    let root = Uuid::new_v4().to_string();
    let proxy = Proxy::bind(home.path(), &backend_path, &root).unwrap();
    let seen = home.path().join("native-reply");
    let mut environment = BTreeMap::from([
        (
            OsString::from("AGENTCHAT_TEST_CODEX_PROXY"),
            proxy.path().as_os_str().into(),
        ),
        (
            OsString::from("AGENTCHAT_TEST_CODEX_TARGET"),
            root.clone().into(),
        ),
        (
            OsString::from("AGENTCHAT_TEST_CODEX_SEEN"),
            seen.as_os_str().into(),
        ),
    ]);
    if failed {
        environment.insert("AGENTCHAT_TEST_CODEX_FAIL".into(), "1".into());
    }
    let args = [
        "--ignored",
        "--exact",
        "codex::proxy::tests::codex_proxy_front_helper",
        "--nocapture",
    ]
    .map(OsString::from);
    let child = NativeChild::spawn(
        &std::env::current_exe().unwrap(),
        home.path(),
        &args,
        &environment,
    )
    .await
    .unwrap();
    let events = proxy.serve(child.proof());
    let (loaded, received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = backend.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let Some(Ok(request)) = socket.next().await else {
            return;
        };
        let request: serde_json::Value = serde_json::from_str(request.to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "thread/resume");
        assert_eq!(request["params"]["threadId"], root);
        // An unloaded native thread becomes available only after this request.
        loaded.send(()).unwrap();
        let reply = if failed {
            json!({"id":7,"error":{"code":-32600,"message":"resume refused"}})
        } else {
            json!({"id":7,"result":{"thread":{"id":root}}})
        };
        socket
            .send(Message::Text(reply.to_string().into()))
            .await
            .unwrap();
        let _ = socket.next().await;
    });
    Startup {
        home,
        child,
        events,
        loaded: received,
        server,
        seen,
    }
}

#[tokio::test]
async fn initial_resume_progresses_before_readiness_without_losing_its_fence() {
    for failed in [false, true] {
        let mut f = startup(failed).await;
        tokio::time::timeout(WAIT, f.events.prepare_initial())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(WAIT, &mut f.loaded)
            .await
            .unwrap()
            .unwrap();
        assert!(!f.home.path().join("hosts/launches").exists());
        assert!(
            f.events.prepare_initial().await.is_err(),
            "initial release is one-use"
        );
        let (connection, transition) = match f.events.try_recv().unwrap() {
            ProxyEvent::Candidate {
                connection_epoch,
                transition_epoch,
                fence,
            } => {
                fence.send(()).unwrap();
                (connection_epoch, transition_epoch)
            }
            _ => panic!("startup lost its original candidate"),
        };
        match tokio::time::timeout(WAIT, f.events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            ProxyEvent::RolledBack {
                connection_epoch,
                transition_epoch,
                release,
            } => {
                assert_eq!(
                    (connection_epoch, transition_epoch),
                    (connection, transition)
                );
                release.send(()).unwrap();
            }
            _ => panic!("same-root success/failure lost its correlated rollback"),
        }
        tokio::time::timeout(WAIT, async {
            while !f.seen.exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        fs::write(f.seen.with_extension("finish"), b"finish").unwrap();
        assert!(
            tokio::time::timeout(WAIT, f.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        f.server.await.unwrap();
    }
}

#[tokio::test]
async fn initial_release_refuses_a_lost_original_owner() {
    let mut f = startup(false).await;
    let mut fence = match tokio::time::timeout(WAIT, f.events.receiver.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ProxyEvent::Candidate { fence, .. } => fence,
        _ => panic!("missing initial candidate"),
    };
    f.child.terminate().await.unwrap();
    assert!(f.events.observer.release_initial(&mut fence).is_err());
    drop(fence);
    assert!(
        tokio::time::timeout(WAIT, &mut f.loaded)
            .await
            .unwrap()
            .is_err()
    );
    f.server.await.unwrap();
}
