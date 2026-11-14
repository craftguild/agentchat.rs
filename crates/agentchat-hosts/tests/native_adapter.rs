//! Real private files/sockets and kernel owners with synthetic native replies.
//! These tests do not validate an actual host or admit a compatibility profile.
use agentchat_core::{
    delivery::{DeliveryPort, Presentation, WakeDispatch, WakeHint, WakeReference},
    model::{Context, HostKind, Phase},
    ports::HostGate,
};
use agentchat_hosts::{
    AdmittedHosts,
    claude::{
        NativeLifecycle, NativeRegistration,
        adapter::NativeAdapter,
        freshness::{Challenge, Snapshot},
        metadata,
        namespace::{OwnerNamespace, TransportEndpoint},
        observer::{NativeProfile, RegisteredOwner},
        process::ProcessIdentity,
        routing,
        wake::{Submission, WakeRecord},
    },
};
use agentchat_protocol::framing;
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::Arc,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixListener,
    task::JoinHandle,
};
use uuid::Uuid;
struct Simulated {
    child: std::process::Child,
    ns: OwnerNamespace,
    native: RegisteredOwner,
    task: JoinHandle<()>,
    launch: Option<agentchat_core::model::LaunchIdentity>,
}
impl Drop for Simulated {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Simulated {
    async fn new(
        home: &std::path::Path,
        cwd: &std::path::Path,
        socket: PathBuf,
        phase: Phase,
    ) -> Self {
        Self::versioned(home, cwd, socket, phase, "2.1.301").await
    }
    async fn versioned(
        home: &std::path::Path,
        cwd: &std::path::Path,
        socket: PathBuf,
        phase: Phase,
        version: &str,
    ) -> Self {
        let child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let expected = std::path::Path::new("/bin/sleep").canonicalize().unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let owner = loop {
            if let Ok(owner) = ProcessIdentity::capture(child.id())
                && owner.executable().canonicalize().ok().as_ref() == Some(&expected)
                && owner.is_current()
            {
                break owner;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "fixture image readiness"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        let ns = OwnerNamespace::open(home, owner.clone()).await.unwrap();
        let nonce = Uuid::new_v4();
        metadata::write(
            &ns.directory().join(format!("gateway-{}.json", ns.id())),
            &TransportEndpoint {
                ownership: None,
                socket: socket.clone(),
                nonce,
            },
        )
        .unwrap();
        let root = Uuid::new_v4();
        let reservation = ns.reserve(root, cwd.into(), Some(nonce)).await.unwrap();
        let native = RegisteredOwner {
            owner,
            registration: NativeRegistration {
                owner_root: ns.id(),
                observer: ns.observer().into(),
                generation: reservation.generation,
                root,
                context: Context {
                    context_id: Uuid::new_v4(),
                    project_id: Uuid::new_v4(),
                    host: HostKind::Claude,
                    host_session_id: root.to_string(),
                    host_version: version.into(),
                    os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                    working_directory: cwd.into(),
                    incarnation: Uuid::new_v4(),
                },
            },
        };
        ns.commit(
            reservation.clone(),
            NativeLifecycle::Project(native.registration.clone()),
        )
        .await
        .unwrap();
        metadata::write(
            &ns.directory().join("native-profile.json"),
            &NativeProfile {
                executable: native.owner.executable(),
                version: version.into(),
            },
        )
        .unwrap();
        metadata::write(
            &ns.directory().join(format!(
                "context-{}.json",
                native.registration.context.context_id
            )),
            &native,
        )
        .unwrap();
        metadata::write(
            &ns.directory()
                .join(format!("native-{}.json", ns.observer())),
            &native,
        )
        .unwrap();
        let h = home.to_owned();
        let n = native.clone();
        let version = version.to_owned();
        ns.publish_current(reservation, move |_| routing::publish(&h, &n))
            .await
            .unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        metadata::write(
            &ns.directory().join(format!("fresh-{}.json", ns.observer())),
            &serde_json::json!({"socket":socket,"observer":ns.observer(),"pid":0}),
        )
        .unwrap();
        let n = native.clone();
        let task = tokio::spawn(async move {
            let mut sequence = 0;
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let request: Challenge = framing::read(&mut reader).await.unwrap().unwrap();
                sequence += 1;
                let answer = Snapshot {
                    nonce: request.nonce,
                    relay_sequence: sequence,
                    native_sequence: sequence,
                    observer: n.registration.observer.clone(),
                    owner_root: n.registration.owner_root,
                    root: n.registration.root,
                    generation: n.registration.generation,
                    epoch: 1,
                    available: true,
                    paused: phase == Phase::Paused,
                    pause_cause: None,
                    approval: phase == Phase::ApprovalBlocked,
                    active_turns: vec![],
                    guard_present: true,
                    surfaces: vec!["terminal".into()],
                    version: version.clone(),
                    unix: 0,
                };
                let mut stream = reader.into_inner();
                framing::write(&mut stream, &answer).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        let launch = if native.registration.context.host_version == "2.1.302" {
            let identity =
                agentchat_hosts::launch::LaunchOwners::capture(child.id(), std::process::id())
                    .unwrap();
            let binding = agentchat_hosts::launch::Binding::new(
                native.registration.context.clone(),
                identity.clone(),
                agentchat_hosts::launch::Endpoint::Claude,
            )
            .unwrap();
            agentchat_hosts::launch::Registry::new(home)
                .publish(&binding)
                .unwrap();
            Some(identity)
        } else {
            None
        };
        Self {
            child,
            ns,
            native,
            task,
            launch,
        }
    }
    fn dispatch(&self) -> WakeDispatch {
        let message_id = Uuid::new_v4();
        let c = &self.native.registration.context;
        WakeDispatch {
            token: Uuid::new_v4(),
            attempt: 1,
            presentation: Presentation::Idle,
            hint: WakeHint {
                version: 1,
                project_id: c.project_id,
                context_id: c.context_id,
                incarnation: c.incarnation,
                agent_id: Uuid::new_v4(),
                batch_id: Uuid::new_v4(),
                events: vec![WakeReference {
                    kind: "message.created".into(),
                    delivery_id: Uuid::new_v4(),
                    dispatch_id: message_id,
                    seq: agentchat_core::sequence::Sequence::new(1).unwrap(),
                    message_id,
                }],
                pending_count: 1,
                preview: "synthetic".into(),
            },
        }
    }
}
fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("agentchat-na-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap()
}
#[tokio::test]
async fn api_eligible_but_unbound_lookup_never_creates_state() {
    let root = root();
    let home = root.path().join("absent");
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    assert!(OwnerNamespace::existing(&home, owner).await.is_err());
    assert!(!home.exists());
    let c = Context {
        context_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        host: HostKind::Claude,
        host_session_id: Uuid::new_v4().to_string(),
        host_version: "2.1.301".into(),
        os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        working_directory: root.path().into(),
        incarnation: Uuid::new_v4(),
    };
    let gate = AdmittedHosts::new(home.clone());
    assert_eq!(
        gate.verify(&c).await.unwrap_err().code,
        agentchat_core::ErrorCode::SessionRequired
    );
    let dispatch = WakeDispatch {
        token: Uuid::new_v4(),
        attempt: 1,
        presentation: Presentation::Idle,
        hint: WakeHint {
            version: 1,
            project_id: c.project_id,
            context_id: c.context_id,
            incarnation: c.incarnation,
            agent_id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            events: vec![],
            pending_count: 0,
            preview: "".into(),
        },
    };
    assert_eq!(
        gate.present(&c, &dispatch).await.unwrap_err().code,
        agentchat_core::ErrorCode::SessionRequired
    );
    assert!(!home.exists());
    assert!(NativeAdapter::new(home.clone()).observe(&c).await.is_err());
    assert!(!home.exists());
}
#[tokio::test]
async fn independent_owners_serialize_signals_without_replacing_inode_and_keep_exact_receipts() {
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let mut owners = vec![];
    for i in 0..8 {
        owners.push(
            Simulated::new(
                &home,
                &cwd,
                root.path().join(format!("r{i}.sock")),
                Phase::Idle,
            )
            .await,
        );
    }
    let before = std::fs::metadata(cwd.join(".agentchat-wakeup")).unwrap();
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "0\n"
    );
    let adapter = Arc::new(NativeAdapter::new(home.clone()));
    let mut attempts = vec![];
    let mut receipts = vec![];
    for (index, owner) in owners.iter().enumerate() {
        let kind = ["message.created", "message.delivery_confirmation_timeout"][index % 2];
        let path = owner.ns.directory().to_owned();
        receipts.push(tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let wake = std::fs::read_dir(&path)
                        .unwrap()
                        .filter_map(|p| p.ok())
                        .find(|p| p.file_name().to_string_lossy().starts_with("wake-"));
                    if let Some(wake) = wake {
                        let w: WakeRecord = metadata::read(&wake.path()).unwrap();
                        assert_eq!(w.hint.events[0].kind, kind);
                        metadata::write(
                            &path.join(format!("submitted-{}.json", w.context_id)),
                            &Submission {
                                accepted: true,
                                token: w.token,
                                context_id: w.context_id,
                                incarnation: w.incarnation,
                                root: w.root,
                                generation: w.generation,
                                unix_ms: 0,
                            },
                        )
                        .unwrap();
                        return w.generation;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap()
        }));
        let c = owner.native.registration.context.clone();
        let mut dispatch = owner.dispatch();
        dispatch.hint.events[0].kind = kind.into();
        let a = adapter.clone();
        attempts.push(tokio::spawn(async move {
            a.present(&c, &dispatch).await.unwrap()
        }));
    }
    for attempt in attempts {
        assert_eq!(attempt.await.unwrap().turn_id, None);
    }
    let mut generations = vec![];
    for receipt in receipts {
        generations.push(receipt.await.unwrap());
    }
    generations.sort();
    assert_eq!(generations, (1..=8).collect::<Vec<_>>());
    let after = std::fs::metadata(cwd.join(".agentchat-wakeup")).unwrap();
    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "8\n"
    );
    // An interrupted/rolled-back counter retains the durable floor. Publication
    // repairs the same inode, and never treats that repair as native acceptance.
    std::fs::write(cwd.join(".agentchat-wakeup"), "0\n").unwrap();
    routing::publish(&home, &owners[0].native).unwrap();
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "8\n"
    );
    let context = &owners[0].native.registration.context;
    let before = std::fs::read(owners[0].ns.directory().join("owner-state.json")).unwrap();
    assert_eq!(adapter.observe(context).await.unwrap().phase, Phase::Idle);
    assert_eq!(
        std::fs::read(owners[0].ns.directory().join("owner-state.json")).unwrap(),
        before
    );
    owners[0]
        .ns
        .reserve(Uuid::new_v4(), cwd, None)
        .await
        .unwrap();
    assert!(adapter.observe(context).await.is_err());
}
#[tokio::test]
async fn paused_native_state_never_publishes_and_foreign_signal_is_preserved() {
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let owner = Simulated::new(&home, &cwd, root.path().join("r.sock"), Phase::Paused).await;
    let adapter = NativeAdapter::new(home.clone());
    assert!(
        adapter
            .present(&owner.native.registration.context, &owner.dispatch())
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "0\n"
    );
    let foreign = root.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join(".agentchat-wakeup"), "foreign sentinel").unwrap();
    let mut native = owner.native.clone();
    native.registration.context.working_directory = foreign.clone();
    native.registration.context.context_id = Uuid::new_v4();
    assert!(routing::publish(&home, &native).is_err());
    assert_eq!(
        std::fs::read_to_string(foreign.join(".agentchat-wakeup")).unwrap(),
        "foreign sentinel"
    );
    assert!(
        !home
            .join(format!(
                "hosts/claude/contexts/{}.json",
                native.registration.context.context_id
            ))
            .exists()
    );
}

#[tokio::test]
async fn unknown_hint_kind_is_refused_without_native_publication() {
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let owner = Simulated::new(&home, &cwd, root.path().join("r.sock"), Phase::Idle).await;
    let before = std::fs::read(owner.ns.directory().join("owner-state.json")).unwrap();
    let mut dispatch = owner.dispatch();
    for kind in ["message.unknown_confirmation", "message.read_timeout"] {
        dispatch.hint.events[0].kind = kind.into();
        assert!(
            NativeAdapter::new(home.clone())
                .present(&owner.native.registration.context, &dispatch)
                .await
                .is_err()
        );
    }
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "0\n"
    );
    assert_eq!(
        std::fs::read(owner.ns.directory().join("owner-state.json")).unwrap(),
        before
    );
    assert!(
        !owner
            .ns
            .directory()
            .join(format!(
                "wake-{}.json",
                owner.native.registration.context.context_id
            ))
            .exists()
    );
}

fn message(owner: &Simulated, addressed: bool) -> agentchat_core::delivery::MessageDispatch {
    use agentchat_core::{
        delivery::{MessageDispatch, MessageInput},
        read::IdentityLabel,
        sequence::Sequence,
    };
    let message_id = Uuid::new_v4();
    MessageDispatch {
        token: Uuid::new_v4(),
        attempt: 1,
        presentation: Presentation::Idle,
        awaiting_acceptance: false,
        frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
        input: MessageInput {
            project_id: owner.native.registration.context.project_id,
            generation: owner
                .launch
                .as_ref()
                .expect("full-input fixture has original launch binding")
                .generation,
            delivery_id: Uuid::new_v4(),

            auto_read: true,
            frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
            dispatch_id: message_id,
            seq: Sequence::new(1).unwrap(),
            message_id,
            thread_id: Uuid::new_v4(),
            author: IdentityLabel {
                actor_id: Uuid::new_v4(),
                kind: agentchat_core::model::ActorKind::Agent,
                handle: "sender".into(),
            },
            channel_id: Uuid::new_v4(),
            channel: "dev".into(),
            body: "\0".repeat(65_536),
            addressed,
        },
    }
}
fn queued_receipt(
    file: &agentchat_hosts::claude::body::InputFile,
) -> agentchat_hosts::claude::body::InputReceipt {
    use agentchat_hosts::claude::body::{InputReceipt, NativeInputResult};
    let id = Uuid::new_v4();
    InputReceipt::from_native(file,NativeInputResult::Queued(serde_json::json!({"root":file.root,"door":"queue","origin":{"kind":"engine"},
        "core":{"plugin":"engine","tier":"core"},"uuid":id,"result":{"uuid":id,"message":{"type":"user","role":"user",
            "content":[{"type":"text","text":file.notice().unwrap()}]}}}))).unwrap()
}
#[tokio::test]
async fn original_known_receipt_survives_removed_routing_and_retired_scope_without_native_io() {
    use agentchat_core::delivery::{MessagePort, MessageProgress};
    use agentchat_hosts::claude::body::{InputFile, PendingInput};
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let mut owner = Simulated::versioned(
        &home,
        &cwd.canonicalize().unwrap(),
        root.path().join("r.sock"),
        Phase::Idle,
        "2.1.302",
    )
    .await;
    let adapter = NativeAdapter::new(home.clone());
    let context = owner.native.registration.context.clone();
    let mut dispatch = message(&owner, true);
    adapter.present_message(&context, &dispatch).await.unwrap();
    dispatch.awaiting_acceptance = true;
    let file = PendingInput::read(&PendingInput::path(
        owner.ns.directory(),
        context.context_id,
    ))
    .unwrap()
    .input(owner.ns.directory())
    .unwrap();
    queued_receipt(&file)
        .publish(
            &owner.ns.metadata_publisher().unwrap(),
            owner.ns.directory(),
        )
        .unwrap();
    let bytes = std::fs::read(file.path(owner.ns.directory())).unwrap();
    routing::remove(&home, &owner.native).unwrap();
    owner
        .ns
        .retire(
            owner.native.registration.root,
            owner.native.registration.generation,
        )
        .await
        .unwrap();
    owner.task.abort();
    assert!(matches!(
        adapter.present_message(&context, &dispatch).await.unwrap(),
        MessageProgress::Accepted(_)
    ));
    let mut output = dispatch.clone();
    output.awaiting_acceptance = false;
    assert!(matches!(
        adapter.present_message(&context, &output).await.unwrap(),
        MessageProgress::NotAccepted(_)
    ));
    let mut wrong = dispatch.clone();
    wrong.input.body = "another body".into();
    assert!(adapter.present_message(&context, &wrong).await.is_err());
    wrong = dispatch.clone();
    wrong.token = Uuid::new_v4();
    assert!(adapter.present_message(&context, &wrong).await.is_err());
    wrong = dispatch.clone();
    wrong.input.generation = Uuid::new_v4();
    assert!(adapter.present_message(&context, &wrong).await.is_err());
    assert_eq!(
        std::fs::read(file.path(owner.ns.directory())).unwrap(),
        bytes
    );
    assert_eq!(
        InputFile::read(&file.path(owner.ns.directory())).unwrap(),
        file
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "1\n"
    );
    owner.child.kill().unwrap();
    owner.child.wait().unwrap();
    assert!(adapter.present_message(&context, &dispatch).await.is_err());
}
#[tokio::test]
async fn no_receipt_keeps_current_publication_waiting_but_retired_scope_is_not_accepted() {
    use agentchat_core::delivery::{MessagePort, MessageProgress};
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let owner = Simulated::versioned(
        &home,
        &cwd.canonicalize().unwrap(),
        root.path().join("r.sock"),
        Phase::Idle,
        "2.1.302",
    )
    .await;
    let adapter = NativeAdapter::new(home.clone());
    let context = &owner.native.registration.context;
    let mut dispatch = message(&owner, true);
    adapter.present_message(context, &dispatch).await.unwrap();
    dispatch.awaiting_acceptance = true;
    assert!(matches!(
        adapter.present_message(context, &dispatch).await.unwrap(),
        MessageProgress::AwaitingAcceptance
    ));
    routing::remove(&home, &owner.native).unwrap();
    owner
        .ns
        .retire(
            owner.native.registration.root,
            owner.native.registration.generation,
        )
        .await
        .unwrap();
    assert!(adapter.present_message(context, &dispatch).await.is_err());
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "1\n"
    );
}
#[tokio::test]
async fn full_input_publication_waits_without_resignal_and_known_queue_acceptance_needs_no_new_native_probe()
 {
    use agentchat_core::delivery::{MessagePort, MessageProgress};
    use agentchat_hosts::claude::body::{InputFile, PendingInput};
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let owner = Simulated::versioned(
        &home,
        &cwd,
        root.path().join("r.sock"),
        Phase::Idle,
        "2.1.302",
    )
    .await;
    let adapter = NativeAdapter::new(home);
    let context = &owner.native.registration.context;
    let mut dispatch = message(&owner, true);
    assert!(matches!(
        adapter.present_message(context, &dispatch).await.unwrap(),
        MessageProgress::AwaitingAcceptance
    ));
    let pointer = PendingInput::read(&PendingInput::path(
        owner.ns.directory(),
        context.context_id,
    ))
    .unwrap();
    let file = pointer.input(owner.ns.directory()).unwrap();
    assert_eq!(file.message().unwrap(), dispatch.input);
    let original = std::fs::metadata(file.path(owner.ns.directory())).unwrap();
    dispatch.awaiting_acceptance = true;
    for _ in 0..3 {
        assert!(matches!(
            adapter.present_message(context, &dispatch).await.unwrap(),
            MessageProgress::AwaitingAcceptance
        ));
    }
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "1\n"
    );
    assert_eq!(
        std::fs::metadata(file.path(owner.ns.directory()))
            .unwrap()
            .ino(),
        original.ino()
    );
    queued_receipt(&file)
        .publish(
            &owner.ns.metadata_publisher().unwrap(),
            owner.ns.directory(),
        )
        .unwrap();
    owner.task.abort(); // A known receipt must not be downgraded by later unavailable native controls.
    assert!(matches!(
        adapter.present_message(context, &dispatch).await.unwrap(),
        MessageProgress::Accepted(_)
    ));
    assert_eq!(
        InputFile::read(&file.path(owner.ns.directory())).unwrap(),
        file
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "1\n"
    );
}
#[tokio::test]
async fn a_committed_successor_retires_only_the_old_scope_files_and_refusal_never_accepts() {
    use agentchat_core::delivery::{MessagePort, MessageProgress};
    use agentchat_hosts::claude::body::{InputFile, InputReceipt, NativeInputResult, PendingInput};
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let owner = Simulated::versioned(
        &home,
        &cwd,
        root.path().join("r.sock"),
        Phase::Idle,
        "2.1.302",
    )
    .await;
    let adapter = NativeAdapter::new(home);
    let context = &owner.native.registration.context;
    let first = message(&owner, true);
    adapter.present_message(context, &first).await.unwrap();
    let first_file = PendingInput::read(&PendingInput::path(
        owner.ns.directory(),
        context.context_id,
    ))
    .unwrap()
    .input(owner.ns.directory())
    .unwrap();
    queued_receipt(&first_file)
        .publish(
            &owner.ns.metadata_publisher().unwrap(),
            owner.ns.directory(),
        )
        .unwrap();
    assert!(matches!(
        adapter.present_message(context, &first).await.unwrap(),
        MessageProgress::Accepted(_)
    ));
    // Shared application ordering supplies only a committed successor. The port
    // owns exact transport retirement, not event selection or cursor advancement.
    let mut second = message(&owner, false);
    second.input.seq = agentchat_core::sequence::Sequence::new(2).unwrap();
    assert!(matches!(
        adapter.present_message(context, &second).await.unwrap(),
        MessageProgress::AwaitingAcceptance
    ));
    assert!(!first_file.path(owner.ns.directory()).exists());
    assert!(!InputReceipt::path(owner.ns.directory(), context.context_id, first.token).exists());
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "2\n"
    );
    let file = PendingInput::read(&PendingInput::path(
        owner.ns.directory(),
        context.context_id,
    ))
    .unwrap()
    .input(owner.ns.directory())
    .unwrap();
    assert_eq!(file.message().unwrap(), second.input);
    assert_eq!(
        InputFile::read(&file.path(owner.ns.directory())).unwrap(),
        file
    );
    InputReceipt::from_native(&file, NativeInputResult::RejectedBeforeEntry)
        .unwrap()
        .publish(
            &owner.ns.metadata_publisher().unwrap(),
            owner.ns.directory(),
        )
        .unwrap();
    second.awaiting_acceptance = true;
    assert!(matches!(
        adapter.present_message(context, &second).await.unwrap(),
        agentchat_core::delivery::MessageProgress::NotAccepted(_)
    ));
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "2\n"
    );
}
#[tokio::test]
async fn full_input_pause_and_positive_owner_loss_never_publish_or_accept_from_surviving_files() {
    use agentchat_core::delivery::MessagePort;
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let mut owner = Simulated::versioned(
        &home,
        &cwd,
        root.path().join("r.sock"),
        Phase::Paused,
        "2.1.302",
    )
    .await;
    let adapter = NativeAdapter::new(home);
    let context = &owner.native.registration.context;
    let dispatch = message(&owner, true);
    assert!(matches!(
        adapter.present_message(context, &dispatch).await.unwrap(),
        agentchat_core::delivery::MessageProgress::NotAccepted(_)
    ));
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "0\n"
    );
    owner.child.kill().unwrap();
    owner.child.wait().unwrap();
    assert!(matches!(
        adapter.present_message(context, &dispatch).await.unwrap(),
        agentchat_core::delivery::MessageProgress::NotAccepted(_)
    ));
    assert_eq!(
        std::fs::read_to_string(cwd.join(".agentchat-wakeup")).unwrap(),
        "0\n"
    );
}

#[tokio::test]
async fn startup_challenge_cannot_substitute_another_live_process_for_its_native_owner() {
    let root = root();
    let home = root.path().join("state");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let owner = Simulated::versioned(
        &home,
        &cwd,
        root.path().join("r.sock"),
        Phase::Paused,
        "2.1.302",
    )
    .await;
    let adapter = NativeAdapter::new(home);
    let context = &owner.native.registration.context;
    let wrong = ProcessIdentity::capture(std::process::id()).unwrap();
    assert!(adapter.observe_owned(context, &wrong).await.is_err());
    let state = adapter
        .observe_owned(context, &owner.native.owner)
        .await
        .unwrap();
    assert_eq!(state.phase, Phase::Paused);
    assert!(state.wake_paused);
}
