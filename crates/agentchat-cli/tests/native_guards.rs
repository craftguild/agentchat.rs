//! Native hook guard processes with explicit synthetic permits; no host admission.
mod support;
use agentchat_cli::claude_gateway::records::{self, Endpoint};
use agentchat_core::model::{Context, HostKind};
use agentchat_hosts::claude::{
    NativeLifecycle, NativeRegistration, namespace::OwnerNamespace, process::ProcessIdentity,
};
use agentchat_runtime::context::{ContextRegistry, ContextSeed};
use serde_json::{Value, json};
use std::sync::Arc;
use std::{
    process::{Output, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use support::Fixture;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
#[derive(Default)]
struct GuardHost(support::launch_bindings::LaunchBindings);
#[async_trait::async_trait]
impl agentchat_core::ports::HostGate for GuardHost {
    async fn launch_identity(
        &self,
        context: &Context,
    ) -> agentchat_core::Result<Option<agentchat_core::model::LaunchIdentity>> {
        Ok(self.0.identity(context))
    }
    fn launch_owner(
        &self,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> agentchat_core::model::LaunchOwnerState {
        self.0.owner(identity)
    }
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::RetainUncertainty,
            ActiveInput::ExactTurnId,
        ))
    }
    fn allows_unmanaged_participation(&self) -> bool {
        true
    }
    async fn verify(
        &self,
        _: &Context,
    ) -> agentchat_core::Result<agentchat_core::ports::HostState> {
        // Explicit synthetic native-shaped descriptor; no production admission.
        Ok(agentchat_core::ports::HostState {
            phase: agentchat_core::model::Phase::Idle,
            active_turn_id: None,
            wake_paused: false,
        })
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
async fn guard(f: &Fixture, mode: &str, input: &[u8]) -> Output {
    let executable = std::env::current_exe().unwrap();
    let mut child = f
        .command(&[
            "internal",
            "claude-guard",
            "--mode",
            mode,
            "--state-directory",
            f.manager.home().to_str().unwrap(),
            "--native-executable",
            executable.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(input).await;
    drop(stdin);
    tokio::time::timeout(Duration::from_secs(6), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
struct Ready {
    f: Fixture,
    ns: OwnerNamespace,
    context: Context,
    permit: Value,
    wake: Value,
    app: agentchat_core::application::Application,
    _client: agentchat_protocol::rpc::Client,
    runtime: agentchat_runtime::daemon::RuntimeHandle,
    task: tokio::task::JoinHandle<agentchat_core::Result<()>>,
}
impl Drop for Ready {
    fn drop(&mut self) {
        self.runtime.shutdown();
        self.task.abort();
    }
}
impl Ready {
    async fn new() -> Self {
        let f = Fixture::new();
        let project = f.initialize().await;
        let root = Uuid::new_v4();
        let context = ContextRegistry::new(f.manager.clone())
            .issue(ContextSeed {
                host: HostKind::Claude,
                host_session_id: root.to_string(),
                host_version: "2.1.301".into(),
                os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                working_directory: f.workspace.clone(),
            })
            .await
            .unwrap();
        let ns = OwnerNamespace::open(
            f.manager.home(),
            ProcessIdentity::capture(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
        let nonce = Uuid::new_v4();
        records::write(
            &records::endpoint(ns.directory(), ns.id()),
            &Endpoint {
                ownership: None,
                socket: ns.directory().join("synthetic.sock"),
                nonce,
            },
        )
        .unwrap();
        let reservation = ns
            .reserve(root, f.workspace.clone(), Some(nonce))
            .await
            .unwrap();
        ns.commit(
            reservation.clone(),
            NativeLifecycle::Project(NativeRegistration {
                owner_root: ns.id(),
                observer: ns.observer().into(),
                generation: reservation.generation,
                root,
                context: context.clone(),
            }),
        )
        .await
        .unwrap();
        let host = Arc::new(GuardHost::default());
        host.0.bind(&context);
        let server = agentchat_runtime::daemon::Server::bind(
            f.manager.clone(),
            project.clone(),
            host.clone(),
            Arc::new(agentchat_core::time::SystemClock),
            agentchat_runtime::daemon::RuntimeOptions::default(),
        )
        .await
        .unwrap();
        let runtime = server.handle();
        let task = tokio::spawn(server.run());
        let app = agentchat_core::application::Application::new(
            f.manager.open(&project).await.unwrap(),
            Arc::new(agentchat_core::time::SystemClock),
            host.clone(),
        );
        app.start_launch(&context, Some("recipient")).await.unwrap();
        let sender = f.context("synthetic-sender").await;
        host.0.bind(&sender);
        app.start_launch(&sender, Some("sender")).await.unwrap();
        let mut client = agentchat_protocol::rpc::Client::connect(
            agentchat_runtime::daemon::RuntimePaths::for_project(&project).socket,
            context.project_id,
        )
        .await
        .unwrap();
        client
            .call(
                Uuid::new_v4(),
                agentchat_protocol::rpc::Call::Attach {
                    context_id: context.context_id,
                },
            )
            .await
            .unwrap();
        app.execute(
            Uuid::new_v4(),
            &sender,
            agentchat_core::application::Mutation::Post {
                channel: Some("dev".into()),
                body: "@recipient guarded test".into(),
            },
        )
        .await
        .unwrap();
        let dispatch = app.prepare_wake(&context).await.unwrap().unwrap();
        app.finish_wake(
            &context,
            &dispatch,
            Ok(agentchat_core::delivery::HostReceipt { turn_id: None }),
        )
        .await
        .unwrap();
        let token = dispatch.token;
        let wake = json!({"root":root,"context_id":context.context_id,"incarnation":context.incarnation,"generation":1234,"token":token,"expected_turn_id":null,"hint":dispatch.hint});
        records::write(
            &ns.directory()
                .join(format!("wake-{}.json", context.context_id)),
            &wake,
        )
        .unwrap();
        let permit = json!({"root":root,"context_id":context.context_id,"incarnation":context.incarnation,"generation":1234,"native_generation":reservation.generation,"token":"native-permit-token","issued":now(),"dispatch":token,"expected_turn_id":null});
        Self {
            f,
            ns,
            context,
            permit,
            wake,
            app,
            _client: client,
            runtime,
            task,
        }
    }
    fn notice(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({"session_id":self.permit["root"],"file_path":self.f.workspace.join(".agentchat-wakeup"),"agentchat_guard_permit":self.permit}))
            .unwrap()
    }
    fn final_input(&self, text: &str) -> Vec<u8> {
        let mut permit = self.permit.clone();
        permit["issued"] = json!(now());
        permit["epoch"] = json!(1);
        permit["phase"] = json!("final");
        serde_json::to_vec(
            &json!({"session_id":permit["root"],"prompt":text,"agentchat_guard_final_permit":permit}),
        )
        .unwrap()
    }
}
#[tokio::test]
async fn exact_notice_is_bounded_deduplicated_and_final_permit_is_separate() {
    let mut r = Ready::new().await;
    r.permit["issued"] = json!(now());
    let output = guard(&r.f, "notice", &r.notice()).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(text.starts_with("AGENTCHAT_TOKEN_NOTICE root="));
    assert!(text.len() <= 8192);
    assert!(text.contains("generation=1234"));
    let receipt =
        r.ns.directory()
            .join(format!("submitted-{}.json", r.context.context_id));
    let value: Value = records::read(&receipt).unwrap();
    assert_eq!(value["accepted"], true);
    assert_eq!(value["token"], r.wake["token"]);
    let repeated = guard(&r.f, "notice", &r.notice()).await;
    assert_eq!(repeated.status.code(), Some(0));
    assert!(repeated.stdout.is_empty() && repeated.stderr.is_empty());
    let valid = guard(&r.f, "final", &r.final_input(&text)).await;
    assert_eq!(valid.status.code(), Some(0));
    assert!(valid.stdout.is_empty());
    let missing = guard(
        &r.f,
        "final",
        &serde_json::to_vec(&json!({"session_id":r.permit["root"],"prompt":text})).unwrap(),
    )
    .await;
    assert_eq!(missing.status.code(), Some(2));
    assert!(missing.stdout.is_empty());
}
#[tokio::test]
async fn stale_or_wrong_root_context_dispatch_and_generation_never_emit_a_notice() {
    let mut r = Ready::new().await;
    for key in [
        "root",
        "context_id",
        "incarnation",
        "dispatch",
        "generation",
        "native_generation",
        "issued",
    ] {
        let old = r.permit.clone();
        r.permit["issued"] = json!(now());
        r.permit[key] = match key {
            "generation" | "native_generation" => json!(999999),
            "issued" => json!(0),
            _ => json!(Uuid::new_v4()),
        };
        let output = guard(&r.f, "notice", &r.notice()).await;
        assert_eq!(output.status.code(), Some(0), "{key}");
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
        r.permit = old;
    }
    assert!(
        !r.ns
            .directory()
            .join(format!("submitted-{}.json", r.context.context_id))
            .exists()
    );
}
#[tokio::test]
async fn an_exact_permit_cannot_emit_for_a_different_changed_file() {
    let r = Ready::new().await;
    let mut input: Value = serde_json::from_slice(&r.notice()).unwrap();
    for path in [
        json!(r.f.workspace.join("other/.agentchat-wakeup")),
        Value::Null,
    ] {
        input["file_path"] = path;
        let output = guard(&r.f, "notice", &serde_json::to_vec(&input).unwrap()).await;
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }
    assert!(
        !r.ns
            .directory()
            .join(format!("submitted-{}.json", r.context.context_id))
            .exists()
    );
}
#[tokio::test]
async fn replacement_endpoint_or_native_intent_invalidates_both_hook_boundaries() {
    let mut r = Ready::new().await;
    r.permit["issued"] = json!(now());
    let text = format!(
        "AGENTCHAT_TOKEN_NOTICE root={} generation=1234 token=native-permit-token.",
        r.permit["root"].as_str().unwrap()
    );
    records::write(
        &records::endpoint(r.ns.directory(), r.ns.id()),
        &Endpoint {
            ownership: None,
            socket: r.ns.directory().join("replacement.sock"),
            nonce: Uuid::new_v4(),
        },
    )
    .unwrap();
    let output = guard(&r.f, "notice", &r.notice()).await;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        guard(&r.f, "final", &r.final_input(&text))
            .await
            .status
            .code(),
        Some(2)
    );
    r.ns.reserve(Uuid::new_v4(), r.f.workspace.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        guard(&r.f, "final", &r.final_input(&text))
            .await
            .status
            .code(),
        Some(2)
    );
}
#[tokio::test]
async fn ordinary_or_unrelated_hooks_do_not_discover_or_create_native_state() {
    let f = Fixture::new();
    for (mode, value) in [
        ("final", json!({"prompt":"ordinary user task"})),
        ("notice", json!({"session_id":Uuid::new_v4()})),
    ] {
        let out = guard(&f, mode, &serde_json::to_vec(&value).unwrap()).await;
        assert_eq!(out.status.code(), Some(0));
        assert!(out.stdout.is_empty() && out.stderr.is_empty());
        assert!(!f.manager.home().exists());
    }
    let invalid = guard(&f, "final", b"{ AGENTCHAT_TOKEN_NOTICE malformed").await;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(!f.manager.home().exists());
}
#[tokio::test]
async fn oversized_hooks_are_classified_without_unbounded_reads_or_state_access() {
    let f = Fixture::new();
    let ordinary = vec![b' '; 1_048_577];
    let out = guard(&f, "final", &ordinary).await;
    assert_eq!(out.status.code(), Some(0));
    let mut owned = b"AGENTCHAT_TOKEN_NOTICE".to_vec();
    owned.resize(1_048_577, b' ');
    let out = guard(&f, "final", &owned).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!f.manager.home().exists());
}

#[tokio::test]
async fn changed_wake_delivery_references_fail_the_snapshot_fence() {
    let mut r = Ready::new().await;
    r.permit["issued"] = json!(now());
    r.wake["hint"]["events"][0]["delivery_id"] = json!(Uuid::new_v4());
    records::write(
        &r.ns
            .directory()
            .join(format!("wake-{}.json", r.context.context_id)),
        &r.wake,
    )
    .unwrap();

    let output = guard(&r.f, "notice", &r.notice()).await;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert!(
        !r.ns
            .directory()
            .join(format!("submitted-{}.json", r.context.context_id))
            .exists()
    );
}

#[tokio::test]
async fn retained_file_and_fresh_native_permits_cannot_outlive_complete_acceptance_leave_or_runtime_loss()
 {
    for case in ["acceptance", "leave", "runtime_loss"] {
        let mut r = Ready::new().await;
        r.permit["issued"] = json!(now());
        let out = guard(&r.f, "notice", &r.notice()).await;
        assert_eq!(out.status.code(), Some(2));
        let notice = String::from_utf8(out.stderr).unwrap();
        assert!(notice.starts_with("AGENTCHAT_TOKEN_NOTICE"));
        match case {
            "acceptance" => {
                let ids = r.wake["hint"]["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| serde_json::from_value(e["delivery_id"].clone()).unwrap())
                    .collect();
                support::accept_native(&r.app, &r.context, ids).await;
            }
            "leave" => {
                r.app
                    .execute(
                        Uuid::new_v4(),
                        &r.context,
                        agentchat_core::application::Mutation::Leave,
                    )
                    .await
                    .unwrap();
            }
            "runtime_loss" => {
                r.runtime.shutdown();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => unreachable!(),
        }
        let out = guard(&r.f, "final", &r.final_input(&notice)).await;
        assert_eq!(out.status.code(), Some(2), "{case}");
        assert!(out.stdout.is_empty() && out.stderr.is_empty());
        // The old durable file deliberately remains in place. Re-signalling it
        // with another short-lived native permit cannot re-authorize the task.
        std::fs::remove_file(
            r.ns.directory()
                .join(format!("submitted-{}.json", r.context.context_id)),
        )
        .unwrap();
        r.permit["issued"] = json!(now());
        let out = guard(&r.f, "notice", &r.notice()).await;
        assert_eq!(out.status.code(), Some(0), "{case}");
        assert!(out.stdout.is_empty() && out.stderr.is_empty());
    }
}
