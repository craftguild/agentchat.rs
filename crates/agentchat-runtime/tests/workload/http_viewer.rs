//! Actual product viewer and rendered SSE audit alongside the native workload.
use super::support::Fixture;
use agentchat_core::{
    ports::Storage,
    read::{Checkpoint, MessageFilter},
    sequence::Cursor,
};
use agentchat_runtime::project::Project;
use agentchat_store::Store;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request,
    body::{Bytes, Incoming},
    client::conn::http1,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpStream,
    process::{Child, Command},
    task::JoinHandle,
};

struct Http {
    body: Incoming,
    connection: JoinHandle<()>,
    pending: Vec<u8>,
}
impl Http {
    async fn open(address: SocketAddr, query: &str, variables: Value, stream: bool) -> Self {
        let socket = TcpStream::connect(address).await.unwrap();
        let (mut sender, connection) = http1::handshake(TokioIo::new(socket)).await.unwrap();
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = Request::post("/graphql")
            .header("host", address.to_string())
            .header("content-type", "application/json")
            .header(
                "accept",
                if stream {
                    "text/event-stream"
                } else {
                    "application/graphql-response+json"
                },
            )
            .body(Full::new(Bytes::from(
                serde_json::to_vec(&json!({"query":query,"variables":variables})).unwrap(),
            )))
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), 200);
        Self {
            body: response.into_body(),
            connection,
            pending: Vec::new(),
        }
    }
    async fn next(&mut self) -> Option<Value> {
        loop {
            if let Some(end) = self.pending.windows(2).position(|v| v == b"\n\n") {
                let bytes: Vec<_> = self.pending.drain(..end + 2).collect();
                assert!(bytes.len() <= 1024 * 1024);
                let frame = std::str::from_utf8(&bytes).unwrap();
                if frame.lines().any(|v| v == "event: complete") {
                    return None;
                }
                if let Some(data) = frame.lines().find_map(|v| v.strip_prefix("data: ")) {
                    let value: Value = serde_json::from_str(data).unwrap();
                    assert!(value.get("errors").is_none(), "{value}");
                    assert!(
                        value
                            .pointer("/extensions/chigiri/delivery/droppedTotal")
                            .is_none_or(Value::is_null)
                    );
                    return Some(value["data"]["changes"].clone());
                }
                continue;
            }
            let frame = self.body.frame().await?.ok()?;
            if let Ok(bytes) = frame.into_data() {
                self.pending.extend_from_slice(&bytes);
            }
            assert!(self.pending.len() <= 2 * 1024 * 1024);
        }
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

pub struct Observed {
    messages: usize,
    channels: usize,
    digest: String,
    maximum_html_bytes: usize,
    frames: usize,
}
pub struct Viewer {
    child: Child,
    task: Option<JoinHandle<Observed>>,
    pub progress: Arc<AtomicI64>,
    closing: Arc<AtomicBool>,
    process_record: Option<PathBuf>,
    identity: Value,
}
impl Drop for Viewer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        // kill_on_drop covers failed assertions without touching shared runtime.
    }
}
fn hash(digest: &mut Sha256, id: &str, seq: &str, body: &str) {
    digest.update(serde_json::to_vec(&[id, seq, body]).unwrap());
    digest.update(b"\n");
}
fn finish(digest: Sha256) -> String {
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub async fn start(
    f: &Fixture,
    project: &Project,
    strict: bool,
    output: Option<&Path>,
) -> Option<Viewer> {
    let Some(binary) = std::env::var_os("AGENTCHAT_WORKLOAD_VIEWER_BINARY") else {
        assert!(
            !strict,
            "manual native workload requires its frozen product viewer binary"
        );
        return None;
    };
    let binary = PathBuf::from(binary);
    assert!(binary.is_absolute() && binary.is_file());
    let stderr = std::fs::File::create(f.root.path().join("viewer-stderr.log")).unwrap();
    let mut child = Command::new(&binary)
        .args(["viewer", "--json"])
        .current_dir(&f.workspace)
        .env("AGENTCHAT_HOME", f.manager.home())
        .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
        .env("AGENTCHAT_CONTEXT", uuid::Uuid::new_v4().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(15), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(ready.len() < 4096);
    std::fs::write(f.root.path().join("viewer-ready.json"), ready.as_bytes()).unwrap();
    let ready: Value = serde_json::from_str(&ready).unwrap();
    assert_eq!(
        ready["project_id"],
        project.identity().id.to_string(),
        "{ready}"
    );
    let address: SocketAddr = ready["result"]["url"]
        .as_str()
        .unwrap()
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .parse()
        .unwrap();
    assert!(address.ip().is_loopback());
    let mut query = Http::open(address, "{project{id checkpoint{seq}}}", json!({}), false).await;
    let body = (&mut query.body).collect().await.unwrap().to_bytes();
    assert!(body.len() <= 4096);
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert!(value.get("errors").is_none(), "{value}");
    assert_eq!(
        value["data"]["project"]["id"],
        project.identity().id.to_string()
    );
    let after = Checkpoint::start(project.identity().id);
    let subscription = "subscription($after:CheckpointInput!){changes(after:$after){after{seq}scan{seq}head{seq}hasMore items{seq kind message{id creationSeq body bodyHtml}}}}";
    let mut stream = Http::open(
        address,
        subscription,
        json!({"after":{"projectId":after.project_id,"seq":after.seq,"eventId":after.event_id}}),
        true,
    )
    .await;
    let progress = Arc::new(AtomicI64::new(0));
    let seen = progress.clone();
    let closing = Arc::new(AtomicBool::new(false));
    let end = closing.clone();
    let task = tokio::spawn(async move {
        let (mut messages, mut channels, mut maximum_html_bytes, mut frames) = (0, 0, 0, 0);
        let mut digest = Sha256::new();
        let mut cursor = 0;
        while let Some(batch) = stream.next().await {
            assert_eq!(batch["after"]["seq"], cursor.to_string());
            let scan = batch["scan"]["seq"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap();
            assert!(scan >= cursor);
            let items = batch["items"].as_array().unwrap();
            assert!(items.len() <= 200);
            let mut previous = cursor;
            for item in items {
                let seq = item["seq"].as_str().unwrap().parse::<i64>().unwrap();
                assert!(seq > previous && seq <= scan);
                previous = seq;
                match item["kind"].as_str().unwrap() {
                    "CHANNEL_CREATED" => channels += 1,
                    "MESSAGE_CREATED" => {
                        let m = &item["message"];
                        let html = m["bodyHtml"].as_str().unwrap();
                        assert!(!html.is_empty());
                        maximum_html_bytes = maximum_html_bytes.max(html.len());
                        hash(
                            &mut digest,
                            m["id"].as_str().unwrap(),
                            m["creationSeq"].as_str().unwrap(),
                            m["body"].as_str().unwrap(),
                        );
                        messages += 1;
                    }
                    other => panic!("unexpected change kind {other}"),
                }
            }
            frames += 1;
            cursor = scan;
            seen.store(scan, Ordering::SeqCst);
        }
        assert!(
            end.load(Ordering::SeqCst),
            "rendered SSE ended before viewer shutdown"
        );
        Observed {
            messages,
            channels,
            digest: finish(digest),
            maximum_html_bytes,
            frames,
        }
    });
    let identity = json!({"state":"running","pid":child.id().unwrap(),"parent_pid":std::process::id(),
        "binary":binary,"binary_sha256":finish(Sha256::new_with_prefix(std::fs::read(&binary).unwrap()))});
    let process_record = output.map(|p| p.join("viewer-process.json"));
    if let Some(path) = &process_record {
        std::fs::write(path, serde_json::to_vec_pretty(&identity).unwrap()).unwrap();
    }
    Some(Viewer {
        child,
        task: Some(task),
        progress,
        closing,
        process_record,
        identity,
    })
}
impl Viewer {
    pub async fn close(mut self) -> Observed {
        self.closing.store(true, Ordering::SeqCst);
        assert!(self.child.try_wait().unwrap().is_none());
        let pid = rustix::process::Pid::from_raw(self.child.id().unwrap() as i32).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
        let status = tokio::time::timeout(Duration::from_secs(6), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "viewer-only shutdown failed: {status}");
        let observed = tokio::time::timeout(Duration::from_secs(3), self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap();
        self.identity["state"] = json!("exited");
        self.identity["exit_code"] = json!(status.code());
        if let Some(path) = &self.process_record {
            std::fs::write(path, serde_json::to_vec_pretty(&self.identity).unwrap()).unwrap();
        }
        observed
    }
}
pub async fn retained(store: &Store, observed: Option<Observed>) -> Value {
    let Some(observed) = observed else {
        return Value::Null;
    };
    let mut snapshot = store.snapshot().await.unwrap();
    let stats = snapshot.statistics().await.unwrap();
    assert_eq!(stats.channels, observed.channels as u64);
    let mut cursor = Cursor::START;
    let mut digest = Sha256::new();
    let mut count = 0;
    loop {
        let rows = snapshot
            .messages(&MessageFilter::default(), cursor, 200)
            .await
            .unwrap();
        if rows.is_empty() {
            break;
        }
        for message in rows {
            hash(
                &mut digest,
                &message.id.to_string(),
                &message.creation_seq.get().to_string(),
                &message.body,
            );
            cursor = message.creation_seq.cursor();
            count += 1;
        }
    }
    assert_eq!(count, observed.messages);
    assert_eq!(finish(digest), observed.digest);
    json!({"scope":"actual product CLI/renderer/managed SSE alongside native workload; no browser",
        "messages":observed.messages,"channels":observed.channels,"frames":observed.frames,
        "complete_body_digest_sha256":observed.digest,"maximum_html_bytes":observed.maximum_html_bytes,
        "finite_queries":1,"viewer_only_shutdown_exit_code":0})
}
