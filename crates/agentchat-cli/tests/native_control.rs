//! Real helper/MCP subprocesses with explicit synthetic native parents only.
mod retirement;
mod support;
use agentchat_cli::claude_gateway::records;
use agentchat_hosts::claude::{namespace::OwnerNamespace, process::ProcessIdentity};
use serde_json::{Value, json};
use std::{
    process::{Output, Stdio},
    time::Duration,
};
use support::{Fixture, mcp::Mcp};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
async fn input(f: &Fixture, bytes: &[u8]) -> Output {
    let parent = std::env::current_exe().unwrap();
    let mut child = f
        .command(&[
            "internal",
            "claude-control",
            "--native-executable",
            parent.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(bytes).await;
    drop(stdin);
    tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
async fn callback(f: &Fixture, action: &str, payload: Value) -> Value {
    let result = input(
        f,
        &serde_json::to_vec(&json!({"action":action,"input":payload})).unwrap(),
    )
    .await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stderr.is_empty());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(value.get("request_id").is_none());
    value
}
async fn reserve(f: &Fixture, root: Uuid) -> Value {
    let parent = std::env::current_exe().unwrap();
    let result = f
        .command(&[
            "internal",
            "claude-namespace",
            "--native-executable",
            parent.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let metadata: Value = serde_json::from_slice(&result.stdout).unwrap();
    callback(f,"reserve",json!({"after_generation":metadata["generation_floor"],"root":root,"cwd":f.workspace,"version":"2.1.301"})).await["reservation"].clone()
}
async fn register(f: &Fixture, reservation: &Value) -> Value {
    callback(
        f,
        "register",
        json!({"reservation":reservation,"version":"2.1.301"}),
    )
    .await["registration"]
        .clone()
}
fn scope(r: &Value) -> Value {
    json!({"root":r["root"],"generation":r["generation"],"version":"2.1.301"})
}
async fn namespace(f: &Fixture) -> OwnerNamespace {
    OwnerNamespace::open(
        f.manager.home(),
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .await
    .unwrap()
}
async fn mcp(f: &Fixture) -> Mcp {
    Mcp::from_child(
        f.command(&["mcp"])
            .env(
                "AGENTCHAT_CLAUDE_NATIVE_EXE",
                std::env::current_exe().unwrap(),
            )
            .env("AGENTCHAT_CLAUDE_NATIVE_VERSION", "2.1.301")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    )
    .await
}
#[tokio::test]
async fn direct_native_callbacks_create_bootstrap_metadata_without_a_project_or_participant() {
    let f = Fixture::new();
    let root = Uuid::new_v4();
    let intent = reserve(&f, root).await;
    let first = register(&f, &intent).await;
    assert_eq!(first["registration"]["stage"], "bootstrap");
    assert_eq!(
        first["registration"]["registration"]["root"],
        root.to_string()
    );
    assert_eq!(register(&f, &intent).await, first);
    assert!(
        !f.workspace.join(".agentchat").exists() && !f.manager.home().join("contexts").exists()
    );
    let ns = namespace(&f).await;
    let stored: Value = records::read(&records::current_owner(ns.directory(), ns.id())).unwrap();
    assert_eq!(stored, first);
    let observation = callback(&f, "observation", scope(&intent)).await;
    assert_eq!(observation["sequence"], 1);
    assert_eq!(observation["current"], true);
    let current=callback(&f,"check",json!({"root":root,"generation":intent["generation"],"cwd":f.workspace,"version":"2.1.301"})).await;
    assert_eq!(current["needed"], false);
    assert_eq!(
        callback(&f, "retire", scope(&intent)).await["retired"],
        true
    );
    assert_eq!(
        callback(&f, "retire", scope(&intent)).await["retired"],
        false
    );
    assert!(!records::current_owner(ns.directory(), ns.id()).exists());
    assert!(ns.current().await.unwrap().is_none());
    assert_eq!(
        callback(&f, "observation", scope(&intent)).await["current"],
        false
    );
}
#[tokio::test]
async fn stale_native_intent_cannot_issue_or_publish_after_a_new_callback() {
    let f = Fixture::new();
    let a = reserve(&f, Uuid::new_v4()).await;
    let b = reserve(&f, Uuid::new_v4()).await;
    let refused = input(
        &f,
        &serde_json::to_vec(
            &json!({"action":"register","input":{"reservation":a,"version":"2.1.301"}}),
        )
        .unwrap(),
    )
    .await;
    assert_eq!(refused.status.code(), Some(4));
    assert!(refused.stdout.is_empty());
    assert!(!f.manager.home().join("contexts").exists());
    let valid = register(&f, &b).await;
    let delayed = input(
        &f,
        &serde_json::to_vec(&json!({"action":"reserve","input":{
            "after_generation":0,"root":a["root"],"cwd":f.workspace,"version":"2.1.301"
        }}))
        .unwrap(),
    )
    .await;
    assert_eq!(delayed.status.code(), Some(4));
    assert!(delayed.stdout.is_empty());
    assert_eq!(
        namespace(&f)
            .await
            .current()
            .await
            .unwrap()
            .unwrap()
            .reservation
            .generation,
        b["generation"].as_u64().unwrap()
    );
    assert_eq!(valid["registration"]["registration"]["root"], b["root"]);
    callback(&f, "retire", scope(&a)).await;
    assert_eq!(
        namespace(&f)
            .await
            .current()
            .await
            .unwrap()
            .unwrap()
            .reservation
            .generation,
        b["generation"].as_u64().unwrap()
    );
}
#[tokio::test]
async fn interrupted_precommit_descriptor_allocation_replays_the_original_intent_identity() {
    use agentchat_core::model::HostKind;
    use agentchat_runtime::context::{ContextRegistry, ContextSeed};
    let f = Fixture::new();
    f.initialize().await;
    let intent = reserve(&f, Uuid::new_v4()).await;
    let registry = ContextRegistry::new(f.manager.clone());
    let proposed = registry
        .prepare(ContextSeed {
            host: HostKind::Claude,
            host_session_id: intent["root"].as_str().unwrap().into(),
            host_version: "2.1.301".into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: f.workspace.clone(),
        })
        .await
        .unwrap();
    let reservation: agentchat_hosts::claude::namespace::Reservation =
        serde_json::from_value(intent.clone()).unwrap();
    let prepared = namespace(&f)
        .await
        .allocate_context(reservation, proposed)
        .await
        .unwrap();
    let prepared = registry.publish(prepared).await.unwrap();
    // The registry write is durable but the native commit has not happened.
    // A new agentchat helper must reuse this record instead of leaking a second UUID.
    assert!(namespace(&f).await.current().await.unwrap().is_none());
    let registered = register(&f, &intent).await;
    assert_eq!(
        registered["registration"]["registration"]["context"],
        serde_json::to_value(&prepared).unwrap()
    );
    assert_eq!(register(&f, &intent).await, registered);
    assert_eq!(
        std::fs::read_dir(f.manager.home().join("contexts"))
            .unwrap()
            .count(),
        1
    );
    callback(&f, "retire", scope(&intent)).await;
}
#[tokio::test]
async fn committed_legacy_context_is_not_reallocated_by_pending_assignment() {
    use agentchat_core::model::HostKind;
    use agentchat_hosts::claude::{NativeLifecycle, NativeRegistration, namespace::Reservation};
    use agentchat_runtime::context::{ContextRegistry, ContextSeed};
    let f = Fixture::new();
    f.initialize().await;
    let raw = reserve(&f, Uuid::new_v4()).await;
    let intent: Reservation = serde_json::from_value(raw.clone()).unwrap();
    let ns = namespace(&f).await;
    let context = ContextRegistry::new(f.manager.clone())
        .issue(ContextSeed {
            host: HostKind::Claude,
            host_session_id: intent.root.to_string(),
            host_version: "2.1.301".into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: f.workspace.clone(),
        })
        .await
        .unwrap();
    let registration = NativeRegistration {
        owner_root: ns.id(),
        observer: ns.observer().into(),
        generation: intent.generation,
        root: intent.root,
        context: context.clone(),
    };
    ns.commit(intent, NativeLifecycle::Project(registration))
        .await
        .unwrap();
    let header = ns.directory().join("owner-state.json");
    let mut legacy: Value = records::read(&header).unwrap();
    legacy["version"] = 1.into();
    assert!(legacy.get("allocation").is_none());
    // This fixture emulates an older owner-header writer. Headers retain their
    // raw versioned representation rather than generic metadata provenance.
    std::fs::write(&header, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let registered = register(&f, &raw).await;
    assert_eq!(
        registered["registration"]["registration"]["context"],
        serde_json::to_value(context).unwrap()
    );
    assert_eq!(
        std::fs::read_dir(f.manager.home().join("contexts"))
            .unwrap()
            .count(),
        1
    );
    callback(&f, "retire", scope(&raw)).await;
}
#[tokio::test]
async fn initialized_native_registration_repairs_projections_without_reissuing_a_descriptor() {
    let f = Fixture::new();
    f.initialize().await;
    let intent = reserve(&f, Uuid::new_v4()).await;
    let first = register(&f, &intent).await;
    assert_eq!(first["registration"]["stage"], "project");
    let ns = namespace(&f).await;
    let context = first["registration"]["registration"]["context"].clone();
    let id = context["context_id"].as_str().unwrap();
    assert_eq!(context["host_session_id"], intent["root"]);
    for name in [
        format!("context-{id}.json"),
        format!("native-{}.json", ns.observer()),
        format!("current-owner-{}.json", ns.id()),
    ] {
        std::fs::remove_file(ns.directory().join(name)).unwrap();
    }
    assert_eq!(register(&f, &intent).await, first);
    let index = ns.directory().join(format!("context-{id}.json"));
    let original = std::fs::read(&index).unwrap();
    let mut foreign: Value = serde_json::from_slice(&original).unwrap();
    foreign["registration"]["root"] = json!(Uuid::new_v4());
    let bytes = serde_json::to_vec(&foreign).unwrap();
    std::fs::write(&index, &bytes).unwrap();
    let refused = input(
        &f,
        &serde_json::to_vec(
            &json!({"action":"register","input":{"reservation":intent,"version":"2.1.301"}}),
        )
        .unwrap(),
    )
    .await;
    assert_eq!(refused.status.code(), Some(4));
    assert_eq!(std::fs::read(&index).unwrap(), bytes);
    std::fs::write(&index, original).unwrap();
    assert_eq!(register(&f, &intent).await, first);
    callback(&f, "retire", scope(&intent)).await;
    assert!(!index.exists());
}
#[tokio::test]
async fn repeated_native_registrations_retire_projections_but_preserve_one_original_body_and_descriptor_history()
 {
    use agentchat_core::{
        delivery::{MessageDispatch, MessageInput, Presentation},
        read::IdentityLabel,
        sequence::Sequence,
    };
    use agentchat_hosts::claude::{
        NativeLifecycle,
        body::{InputFile, InputHead, PendingInput},
    };
    let f = Fixture::new();
    f.initialize().await;
    let root = Uuid::new_v4();
    let mut intent = reserve(&f, root).await;
    let original = register(&f, &intent).await;
    let lifecycle: records::LifecycleOwner = serde_json::from_value(original).unwrap();
    let NativeLifecycle::Project(first) = lifecycle.registration else {
        panic!("project native scope");
    };
    let ns = namespace(&f).await;
    let publisher = ns.metadata_publisher().unwrap();
    // Physical metadata retention only: this synthetic legacy-version scope
    // does not establish a native full-input consumer or application launch.
    let message_id = Uuid::new_v4();
    let dispatch = MessageDispatch {
        token: Uuid::new_v4(),
        attempt: 1,
        presentation: Presentation::Idle,
        awaiting_acceptance: false,
        frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
        input: MessageInput {
            project_id: first.context.project_id,
            generation: Uuid::new_v4(),
            delivery_id: Uuid::new_v4(),

            auto_read: true,
            frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
            dispatch_id: message_id,
            seq: Sequence::new(2).unwrap(),
            message_id,
            thread_id: Uuid::new_v4(),
            author: IdentityLabel {
                kind: agentchat_core::model::ActorKind::Agent,
                actor_id: Uuid::new_v4(),
                handle: "sender".into(),
            },
            channel_id: Uuid::new_v4(),
            channel: "dev".into(),
            body: "Retained original transport body".into(),
            addressed: false,
        },
    };
    let file = InputFile::new(&first.context, root, first.generation, &dispatch).unwrap();
    file.publish(&publisher, ns.directory()).unwrap();
    InputHead::new(&file)
        .unwrap()
        .publish(&publisher, ns.directory())
        .unwrap();
    PendingInput::new(&file, 1)
        .unwrap()
        .publish(&publisher, ns.directory())
        .unwrap();
    let mut current = first.clone();
    for _ in 0..10 {
        intent = reserve(&f, root).await;
        assert!(
            !ns.directory()
                .join(format!("context-{}.json", current.context.context_id))
                .exists()
        );
        assert!(
            !f.manager
                .home()
                .join("hosts/claude/contexts")
                .join(format!("{}.json", current.context.context_id))
                .exists()
        );
        let lifecycle: records::LifecycleOwner =
            serde_json::from_value(register(&f, &intent).await).unwrap();
        let NativeLifecycle::Project(next) = lifecycle.registration else {
            panic!("project native scope");
        };
        current = next;
        assert_eq!(
            InputHead::read(ns.directory())
                .unwrap()
                .unwrap()
                .input(ns.directory())
                .unwrap(),
            file
        );
        let names: Vec<_> = std::fs::read_dir(ns.directory())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names.iter().filter(|n| n.starts_with("context-")).count(),
            1
        );
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("pending-input-"))
                .count(),
            0
        );
        assert_eq!(
            std::fs::read_dir(f.manager.home().join("hosts/claude/contexts"))
                .unwrap()
                .count(),
            1
        );
    }
    assert_eq!(
        std::fs::read_dir(f.manager.home().join("contexts"))
            .unwrap()
            .count(),
        11
    );
    assert!(file.path(ns.directory()).exists());
}
#[tokio::test]
async fn a_new_transport_requires_fresh_native_generation_and_keeps_pending_call_retry_exact() {
    let f = Fixture::new();
    let root = Uuid::new_v4();
    let old = reserve(&f, root).await;
    register(&f, &old).await;
    let mut client = mcp(&f).await;
    let ns = namespace(&f).await;
    let endpoint = ns.transport().await.unwrap().unwrap();
    assert_eq!(
        client
            .call("agentchat_whoami", json!({"project_path":f.workspace}))
            .await["error"]["code"],
        "session_required"
    );
    assert_eq!(callback(&f,"check",json!({"root":root,"generation":old["generation"],"cwd":f.workspace,"version":"2.1.301"})).await["needed"],true);
    let fresh = reserve(&f, root).await;
    assert!(fresh["generation"].as_u64() > old["generation"].as_u64());
    assert_eq!(fresh["transport_nonce"], endpoint.nonce.to_string());
    register(&f, &fresh).await;
    let record = json!({"scope":scope(&fresh),"tool_name":"mcp__agentchat__agentchat_whoami","tool_use_id":"owned_call"});
    callback(&f, "record", record.clone()).await;
    callback(&f, "record", record.clone()).await;
    let header: Value =
        serde_json::from_slice(&std::fs::read(ns.directory().join("owner-state.json")).unwrap())
            .unwrap();
    assert_eq!(header["call_sequence"], 1);
    assert_eq!(header["calls"].as_array().unwrap().len(), 1);
    let result=client.request("tools/call",json!({"name":"agentchat_whoami","arguments":{"project_path":f.workspace},"_meta":{"claudecode/toolUseId":"owned_call"}})).await;
    assert_eq!(
        result["result"]["structuredContent"]["error"]["code"],
        "session_required"
    );
    let replay = input(
        &f,
        &serde_json::to_vec(&json!({"action":"record","input":record})).unwrap(),
    )
    .await;
    assert_eq!(replay.status.code(), Some(4));
    assert!(replay.stdout.is_empty());
    callback(
        &f,
        "revoke",
        json!({"scope":scope(&fresh),"tool_use_id":"owned_call"}),
    )
    .await;
    client.close().await;
    let mut again = mcp(&f).await;
    assert_ne!(ns.transport().await.unwrap().unwrap().nonce, endpoint.nonce);
    let old_record=input(&f,&serde_json::to_vec(&json!({"action":"record","input":{"scope":scope(&fresh),"tool_name":"mcp__agentchat__agentchat_whoami","tool_use_id":"old_transport"}})).unwrap()).await;
    assert_eq!(old_record.status.code(), Some(4));
    assert_eq!(
        again
            .call("agentchat_whoami", json!({"project_path":f.workspace}))
            .await["error"]["code"],
        "session_required"
    );
    let successor = reserve(&f, root).await;
    register(&f, &successor).await;
    assert!(successor["generation"].as_u64() > fresh["generation"].as_u64());
    again.close().await;
    assert!(!f.workspace.join(".agentchat").exists());
}
#[tokio::test]
async fn malformed_native_inputs_refuse_before_state_creation() {
    for value in [
        json!({"action":"unknown","input":{}}),
        json!({"action":"reserve","input":{"after_generation":0,"root":Uuid::new_v4(),"cwd":"relative","version":"2.1.301"}}),
        json!({"action":"reserve","input":{"after_generation":0,"root":Uuid::new_v4(),"cwd":"/","version":"2.1.301","context_id":Uuid::new_v4()}}),
    ] {
        let f = Fixture::new();
        let result = input(&f, &serde_json::to_vec(&value).unwrap()).await;
        assert!(!result.status.success());
        assert!(result.stdout.is_empty() && !result.stderr.is_empty());
        assert!(!f.manager.home().exists());
    }
}
#[tokio::test]
async fn wrong_parent_and_invalid_internal_arguments_keep_stdout_and_state_clean() {
    let f = Fixture::new();
    let wrong = f
        .command(&[
            "internal",
            "claude-control",
            "--native-executable",
            env!("CARGO_BIN_EXE_agentchat"),
        ])
        .output()
        .await
        .unwrap();
    assert_eq!(wrong.status.code(), Some(4));
    assert!(wrong.stdout.is_empty());
    assert!(!f.manager.home().exists());
    let invalid = f
        .command(&["internal", "claude-control", "--native-executable"])
        .output()
        .await
        .unwrap();
    assert!(!invalid.status.success() && invalid.stdout.is_empty());
}
#[tokio::test]
async fn unclosed_native_input_has_a_finite_cancellable_deadline() {
    let f = Fixture::new();
    let parent = std::env::current_exe().unwrap();
    let mut child = f
        .command(&[
            "internal",
            "claude-control",
            "--native-executable",
            parent.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let held = child.stdin.take().unwrap();
    // The input deadline is two seconds after descriptor validation. Keep the
    // black-box bound separate from concurrent process/image-capture overhead.
    let result = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    drop(held);
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty() && !result.stderr.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("input timed out"));
    assert!(!f.manager.home().exists());
}
#[tokio::test]
async fn managed_native_stream_announces_new_endpoint_incarnations_without_creating_a_root() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let f = Fixture::new();
    let parent = std::env::current_exe().unwrap();
    let mut stream = f
        .command(&[
            "internal",
            "claude-stream",
            "--native-executable",
            parent.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    let pid = rustix::process::Pid::from_raw(stream.id().unwrap() as i32).unwrap();
    let mut output = BufReader::new(stream.stdout.take().unwrap());
    let first = mcp(&f).await;
    let first_owner = ProcessIdentity::capture(first.process_id()).unwrap();
    let ns = namespace(&f).await;
    async fn notice(
        reader: &mut BufReader<tokio::process::ChildStdout>,
        lost: Option<(&Value, &ProcessIdentity)>,
    ) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        let mut loss_seen = false;
        loop {
            let mut line = String::new();
            tokio::time::timeout_at(deadline, reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            assert!(line.len() < 1024);
            let value: Value = serde_json::from_str(&line).unwrap();
            assert!(value.get("root").is_none());
            if value["event"] == "transport_lost" {
                let (nonce, owner) = lost.expect("a live original endpoint cannot report loss");
                assert!(!loss_seen);
                loss_seen = true;
                assert_eq!(&value["endpoint_nonce"], nonce);
                assert_eq!(
                    owner.birth_state(),
                    agentchat_hosts::claude::process::BirthState::Gone
                );
                continue;
            }
            assert_eq!(value["event"], "transport_changed");
            return value;
        }
    }
    let first_notice = notice(&mut output, None).await;
    assert_eq!(
        first_notice["endpoint_nonce"],
        ns.transport().await.unwrap().unwrap().nonce.to_string()
    );
    first.close().await;
    let second = mcp(&f).await;
    // Closing the original helper may legitimately emit its proved loss before
    // the replacement appears. It cannot be mistaken for a changed endpoint.
    let second_notice = notice(
        &mut output,
        Some((&first_notice["endpoint_nonce"], &first_owner)),
    )
    .await;
    assert_ne!(
        first_notice["endpoint_nonce"],
        second_notice["endpoint_nonce"]
    );
    assert!(ns.current().await.unwrap().is_none());
    let header: Value =
        serde_json::from_slice(&std::fs::read(ns.directory().join("owner-state.json")).unwrap())
            .unwrap();
    assert_eq!(header["generation"], 0);
    assert_eq!(header["call_sequence"], 0);
    assert_eq!(header["observation_sequence"], 0);
    second.close().await;
    // The retained Child is unreaped, so this PID still names our owned helper.
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(4), stream.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(result.status.success() && result.stderr.is_empty());
    assert!(
        !ns.directory()
            .join(format!("fresh-{}.json", ns.observer()))
            .exists()
    );
    assert!(
        !f.workspace.join(".agentchat").exists() && !f.manager.home().join("contexts").exists()
    );
}

#[tokio::test]
async fn a_failed_native_observation_releases_its_challenge_for_the_next_exact_call() {
    let f = Fixture::new();
    let mut client = mcp(&f).await;
    let intent = reserve(&f, Uuid::new_v4()).await;
    register(&f, &intent).await;
    for id in ["first_unavailable", "second_unavailable"] {
        callback(
            &f,
            "record",
            json!({"scope":scope(&intent),"tool_name":"mcp__agentchat__agentchat_whoami","tool_use_id":id}),
        )
        .await;
        let response=client.request("tools/call",json!({"name":"agentchat_whoami","arguments":{"project_path":f.workspace},"_meta":{"claudecode/toolUseId":id}})).await;
        // No native producer is installed in this synthetic fixture. Its missing
        // observation must refuse each call without lending a pending challenge
        // or returning Busy until that failed challenge's deadline expires.
        assert_eq!(
            response["result"]["structuredContent"]["error"]["code"],
            "session_required"
        );
        callback(
            &f,
            "revoke",
            json!({"scope":scope(&intent),"tool_use_id":id}),
        )
        .await;
    }
    client.close().await;
    assert!(!f.workspace.join(".agentchat").exists());
}
