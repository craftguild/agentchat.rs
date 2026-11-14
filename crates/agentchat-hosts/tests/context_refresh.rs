//! Actual private relay/peer credentials; native SDK facts are explicitly mocked.
use agentchat_core::model::{Context, HostKind};
use agentchat_hosts::{
    claude::{
        NativeLifecycle, NativeRegistration, namespace::OwnerNamespace, observer::relay,
        process::ProcessIdentity,
    },
    launch::{Binding, Endpoint, LaunchOwners, Registry},
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    net::UnixStream,
};
use uuid::Uuid;

struct Owner(Child);
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    root: tempfile::TempDir,
    _owner: Owner,
    namespace: OwnerNamespace,
    binding: Binding,
    generation: u64,
    socket: PathBuf,
    output: BufReader<DuplexStream>,
    worker: tokio::task::JoinHandle<agentchat_core::Result<()>>,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let owner = Owner(Command::new("/bin/sleep").arg("60").spawn().unwrap());
        let expected = std::path::Path::new("/bin/sleep").canonicalize().unwrap();
        let token = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(token) = ProcessIdentity::capture(owner.0.id())
                    && token.executable() == expected
                {
                    break token;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let namespace = OwnerNamespace::open(root.path(), token).await.unwrap();
        let native_root = Uuid::new_v4();
        let reservation = namespace
            .reserve(native_root, root.path().into(), None)
            .await
            .unwrap();
        let context = Context {
            context_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            host: HostKind::Claude,
            host_session_id: native_root.to_string(),
            host_version: "2.1.302".into(),
            os_profile: "explicit-synthetic".into(),
            working_directory: root.path().into(),
            incarnation: Uuid::new_v4(),
        };
        namespace
            .commit(
                reservation.clone(),
                NativeLifecycle::Project(NativeRegistration {
                    owner_root: namespace.id(),
                    observer: namespace.observer().into(),
                    generation: reservation.generation,
                    root: native_root,
                    context: context.clone(),
                }),
            )
            .await
            .unwrap();
        let binding = Binding::new(
            context,
            LaunchOwners::capture(owner.0.id(), std::process::id()).unwrap(),
            Endpoint::Claude,
        )
        .unwrap();
        Registry::new(root.path()).publish(&binding).unwrap();
        let (writer, reader) = tokio::io::duplex(8192);
        let ns = namespace.clone();
        let worker = tokio::spawn(async move { relay::serve_managed(ns, writer).await });
        let path = namespace
            .directory()
            .join(format!("fresh-{}.json", namespace.observer()));
        let socket = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = std::fs::read(&path)
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                    && let Some(path) = value["socket"].as_str()
                {
                    break PathBuf::from(path);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        Self {
            root,
            _owner: owner,
            namespace,
            binding,
            generation: reservation.generation,
            socket,
            output: BufReader::new(reader),
            worker,
        }
    }
    fn request(&self) -> Value {
        json!({"refresh_context":self.binding.context.context_id,"native_generation":self.generation,"launch_generation":self.binding.identity.generation})
    }
    async fn request_value(&self, value: Value) -> Value {
        request(&self.socket, value).await
    }
    async fn close(self) {
        self.worker.abort();
        let _ = self.worker.await;
    }
}
async fn request(path: &std::path::Path, value: Value) -> Value {
    let mut peer = UnixStream::connect(path).await.unwrap();
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    peer.write_all(&bytes).await.unwrap();
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(peer).read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    serde_json::from_str(&line).unwrap()
}
#[tokio::test]
async fn original_supervisor_can_request_one_separate_hint_without_changing_native_or_application_state()
 {
    let mut f = Fixture::new().await;
    let before = f.namespace.read_current().await.unwrap();
    let response = f.request_value(f.request()).await;
    assert_eq!(response["accepted"], true);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(2), f.output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let event: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        event,
        json!({"event":"context_refresh","context_id":f.binding.context.context_id,"native_generation":f.generation})
    );
    assert_eq!(f.namespace.read_current().await.unwrap(), before);
    assert!(!f.root.path().join("agentchat.sqlite3").exists());
    assert!(!f.root.path().join("contexts").exists());
    f.close().await;
}
#[tokio::test]
async fn stale_scope_foreign_generation_and_retired_namespace_cannot_request_refresh() {
    let mut f = Fixture::new().await;
    for field in ["refresh_context", "native_generation", "launch_generation"] {
        let mut request = f.request();
        request[field] = if field == "native_generation" {
            json!(f.generation + 1)
        } else {
            json!(Uuid::new_v4())
        };
        assert_eq!(f.request_value(request).await["accepted"], false, "{field}");
    }
    assert!(
        Registry::new(f.root.path())
            .record_root_change(&f.binding, Uuid::new_v4())
            .is_err(),
        "a supervisor cannot manufacture a native callback's departure proof"
    );
    f.namespace
        .retire(
            Uuid::parse_str(&f.binding.context.host_session_id).unwrap(),
            f.generation,
        )
        .await
        .unwrap();
    assert_eq!(f.request_value(f.request()).await["accepted"], false);
    let mut line = String::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), f.output.read_line(&mut line))
            .await
            .is_err()
    );
    f.close().await;
}
#[test]
#[ignore = "owned stranger process; its parent checks actual Unix peer PID fencing"]
fn foreign_refresh_peer() {
    let path = PathBuf::from(std::env::var_os("AGENTCHAT_REFRESH_SOCKET").unwrap());
    let request: Value =
        serde_json::from_str(&std::env::var("AGENTCHAT_REFRESH_REQUEST").unwrap()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime.block_on(self::request(&path, request));
    assert_eq!(response["accepted"], false);
}
#[tokio::test]
async fn a_different_same_user_process_cannot_borrow_the_original_supervisor_binding() {
    let mut f = Fixture::new().await;
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "foreign_refresh_peer"])
        .env("AGENTCHAT_REFRESH_SOCKET", &f.socket)
        .env(
            "AGENTCHAT_REFRESH_REQUEST",
            serde_json::to_string(&f.request()).unwrap(),
        )
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut line = String::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), f.output.read_line(&mut line))
            .await
            .is_err()
    );
    f.close().await;
}
