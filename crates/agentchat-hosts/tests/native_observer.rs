//! Synthetic native replies over real private sockets; never profile admission.
use agentchat_core::{
    ErrorCode,
    model::{Context, HostKind, Phase},
};
use agentchat_hosts::claude::{
    NativeRegistration,
    freshness::{Challenge, Snapshot},
    observer::{NativeObserver, NativeProfile, RegisteredOwner},
    process::ProcessIdentity,
};
use agentchat_protocol::framing;
use std::{
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixListener,
    sync::oneshot,
};
use uuid::Uuid;

fn write(path: &Path, value: &impl serde::Serialize) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    f.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
}
struct Fixture {
    root: tempfile::TempDir,
    current: RegisteredOwner,
    profile: NativeProfile,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("agentchat-observe-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap();
        let id = Uuid::new_v4();
        let current = RegisteredOwner {
            registration: NativeRegistration {
                owner_root: id,
                observer: "synthetic-observer".into(),
                generation: 1,
                root: id,
                context: Context {
                    context_id: Uuid::new_v4(),
                    project_id: Uuid::new_v4(),
                    incarnation: Uuid::new_v4(),
                    host: HostKind::Claude,
                    host_session_id: id.to_string(),
                    host_version: "synthetic".into(),
                    os_profile: "synthetic".into(),
                    working_directory: PathBuf::from("/owned/project"),
                },
            },
            owner: ProcessIdentity::capture(std::process::id()).unwrap(),
        };
        let profile = NativeProfile {
            executable: std::env::current_exe().unwrap(),
            version: "synthetic".into(),
        };
        let f = Self {
            root,
            current,
            profile,
        };
        f.publish(&f.current);
        f
    }
    fn publish(&self, value: &RegisteredOwner) {
        write(
            &self.root.path().join(format!(
                "context-{}.json",
                value.registration.context.context_id
            )),
            value,
        );
        write(&self.current_path(), value);
    }
    fn current_path(&self) -> PathBuf {
        self.root.path().join(format!(
            "native-{}.json",
            self.current.registration.observer
        ))
    }
    fn listen(&self) -> UnixListener {
        let socket = self.root.path().join("relay.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        write(
            &self.root.path().join("fresh-synthetic-observer.json"),
            &serde_json::json!({"observer":"synthetic-observer","socket":socket,"pid":0}),
        );
        listener
    }
    async fn open(&self) -> Arc<NativeObserver> {
        Arc::new(
            NativeObserver::open(self.root.path().into(), self.profile.clone())
                .await
                .unwrap(),
        )
    }
}

#[tokio::test]
async fn dead_kernel_owners_release_capacity_without_reopening_their_old_contexts() {
    struct Children(Vec<std::process::Child>);
    impl Drop for Children {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let f = Fixture::new();
    let listener = f.listen();
    let executable = PathBuf::from("/bin/sleep").canonicalize().unwrap();
    let mut children = Children(Vec::new());
    let mut registrations = Vec::new();
    for n in 0..33 {
        let child = std::process::Command::new(&executable)
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();
        children.0.push(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let owner = loop {
            if let Ok(owner) = ProcessIdentity::capture(pid)
                && owner.executable() == executable
                && owner.is_current()
            {
                break owner;
            }
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        let mut r = f.current.clone();
        r.owner = owner;
        r.registration.observer = format!("synthetic-{n}");
        r.registration.root = Uuid::new_v4();
        r.registration.owner_root = r.registration.root;
        r.registration.context.context_id = Uuid::new_v4();
        r.registration.context.incarnation = Uuid::new_v4();
        r.registration.context.host_session_id = r.registration.root.to_string();
        write(
            &f.root.path().join(format!(
                "context-{}.json",
                r.registration.context.context_id
            )),
            &r,
        );
        write(
            &f.root
                .path()
                .join(format!("native-{}.json", r.registration.observer)),
            &r,
        );
        write(
            &f.root
                .path()
                .join(format!("fresh-{}.json", r.registration.observer)),
            &serde_json::json!({"observer":r.registration.observer,"socket":f.root.path().join("relay.sock"),"pid":0}),
        );
        registrations.push(r);
    }
    let profile = NativeProfile {
        executable,
        version: "synthetic".into(),
    };
    let observer = NativeObserver::open(f.root.path().into(), profile)
        .await
        .unwrap();
    let responses = registrations.clone();
    let responder = tokio::spawn(async move {
        for r in responses {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut out) = stream.into_split();
            let q = framing::read::<_, Challenge>(&mut BufReader::new(read))
                .await
                .unwrap()
                .unwrap();
            framing::write(&mut out, &answer(&r, q, 1)).await.unwrap();
            out.shutdown().await.unwrap();
        }
    });
    for r in &registrations[..32] {
        assert_eq!(
            observer
                .verify(&r.registration.context)
                .await
                .unwrap()
                .phase,
            Phase::Idle
        );
    }
    children.0[0].kill().unwrap();
    children.0[0].wait().unwrap();
    assert_eq!(
        observer
            .verify(&registrations[32].registration.context)
            .await
            .unwrap()
            .phase,
        Phase::Idle
    );
    assert_eq!(
        observer
            .verify(&registrations[0].registration.context)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    responder.await.unwrap();
}
fn answer(r: &RegisteredOwner, q: Challenge, n: u64) -> Snapshot {
    Snapshot {
        nonce: q.nonce,
        relay_sequence: n,
        native_sequence: n,
        observer: r.registration.observer.clone(),
        owner_root: r.registration.owner_root,
        root: r.registration.root,
        generation: r.registration.generation,
        epoch: 1,
        available: true,
        paused: false,
        pause_cause: None,
        approval: false,
        active_turns: vec![],
        guard_present: true,
        surfaces: vec!["terminal".into()],
        version: r.registration.context.host_version.clone(),
        unix: 0,
    }
}
#[tokio::test]
async fn a_historical_index_cannot_authorize_a_reply_after_clear() {
    let f = Fixture::new();
    let listener = f.listen();
    let observer = f.open().await;
    let context = f.current.registration.context.clone();
    let querying = observer.clone();
    let pending = tokio::spawn(async move { querying.verify(&context).await });
    let (stream, _) = listener.accept().await.unwrap();
    let (read, mut out) = stream.into_split();
    let q = framing::read::<_, Challenge>(&mut BufReader::new(read))
        .await
        .unwrap()
        .unwrap();
    let mut next = f.current.clone();
    next.registration.generation += 1;
    next.registration.root = Uuid::new_v4();
    next.registration.context.context_id = Uuid::new_v4();
    next.registration.context.incarnation = Uuid::new_v4();
    next.registration.context.host_session_id = next.registration.root.to_string();
    f.publish(&next);
    framing::write(&mut out, &answer(&f.current, q, 1))
        .await
        .unwrap();
    out.shutdown().await.unwrap();
    assert_eq!(
        pending.await.unwrap().unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        observer
            .verify(&f.current.registration.context)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    let context = next.registration.context.clone();
    let querying = observer.clone();
    let pending = tokio::spawn(async move { querying.verify(&context).await });
    let (stream, _) = listener.accept().await.unwrap();
    let (read, mut out) = stream.into_split();
    let q = framing::read::<_, Challenge>(&mut BufReader::new(read))
        .await
        .unwrap()
        .unwrap();
    framing::write(&mut out, &answer(&next, q, 2))
        .await
        .unwrap();
    out.shutdown().await.unwrap();
    assert_eq!(pending.await.unwrap().unwrap().phase, Phase::Idle);
}
#[tokio::test]
async fn unavailable_transport_is_not_cached_and_later_real_requests_can_recover() {
    let f = Fixture::new();
    let observer = f.open().await;
    let c = f.current.registration.context.clone();
    assert!(observer.verify(&c).await.is_err());
    let listener = f.listen();
    let current = f.current.clone();
    let responder = tokio::spawn(async move {
        for n in [10, 10, 11] {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut out) = stream.into_split();
            let q = framing::read::<_, Challenge>(&mut BufReader::new(read))
                .await
                .unwrap()
                .unwrap();
            framing::write(&mut out, &answer(&current, q, n))
                .await
                .unwrap();
            out.shutdown().await.unwrap();
        }
    });
    assert_eq!(observer.verify(&c).await.unwrap().phase, Phase::Idle);
    assert!(observer.verify(&c).await.is_err());
    assert_eq!(observer.verify(&c).await.unwrap().phase, Phase::Idle);
    responder.await.unwrap();
}
#[tokio::test]
async fn removing_the_current_record_during_a_wait_refuses_a_valid_cached_answer() {
    let f = Fixture::new();
    let listener = f.listen();
    let observer = f.open().await;
    let c = f.current.registration.context.clone();
    let p = tokio::spawn(async move { observer.verify(&c).await });
    let (stream, _) = listener.accept().await.unwrap();
    let (read, mut out) = stream.into_split();
    let q = framing::read::<_, Challenge>(&mut BufReader::new(read))
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(f.current_path()).unwrap();
    framing::write(&mut out, &answer(&f.current, q, 1))
        .await
        .unwrap();
    out.shutdown().await.unwrap();
    assert!(p.await.unwrap().is_err());
}
#[tokio::test]
async fn the_frame_bound_is_applied_before_json_decoding_and_does_not_lend_a_retry() {
    let f = Fixture::new();
    let listener = f.listen();
    let observer = f.open().await;
    let current = f.current.clone();
    let (sent, received) = oneshot::channel();
    let responder = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut out) = stream.into_split();
        let _ = framing::read::<_, Challenge>(&mut BufReader::new(read))
            .await
            .unwrap();
        let _ = out.write_all(&vec![b'x'; 8193]).await;
        let _ = out.shutdown().await;
        sent.send(()).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut out) = stream.into_split();
        let q = framing::read::<_, Challenge>(&mut BufReader::new(read))
            .await
            .unwrap()
            .unwrap();
        framing::write(&mut out, &answer(&current, q, 2))
            .await
            .unwrap();
        out.shutdown().await.unwrap();
    });
    assert!(
        observer
            .verify(&f.current.registration.context)
            .await
            .is_err()
    );
    received.await.unwrap();
    assert_eq!(
        observer
            .verify(&f.current.registration.context)
            .await
            .unwrap()
            .phase,
        Phase::Idle
    );
    responder.await.unwrap();
}
