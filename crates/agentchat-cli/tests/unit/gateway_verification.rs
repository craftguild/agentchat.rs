//! Bounded native-control waits preserve one-use proof and current-root fences.
use super::super::{Diagnostics, Native, Projection};
use super::*;
use agentchat_hosts::claude::{
    CallProofs, NativeBootstrap, NativeCall, NativeLifecycle,
    freshness::{Challenge, Freshness, Snapshot},
    process::ProcessIdentity,
};
use agentchat_mcp::Conversations;
use agentchat_protocol::framing;
use agentchat_runtime::project::ProjectManager;
use serde_json::json;
use std::{
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    sync::Arc,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixListener,
    sync::Mutex,
};
use uuid::Uuid;

const TOOL_ID: &str = "toolu_native_wait";

struct Fixture {
    base: tempfile::TempDir,
    bridge: Bridge,
    lifecycle: NativeBootstrap,
}

impl Fixture {
    fn new() -> Self {
        let base = tempfile::Builder::new()
            .prefix("agentchat-call-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap();
        let manager = ProjectManager::new(base.path().join("state")).unwrap();
        let owner = ProcessIdentity::capture(std::process::id()).unwrap();
        let locator = Uuid::new_v4();
        let lifecycle = NativeBootstrap {
            owner_root: locator,
            observer: "observer".into(),
            generation: 1,
            root: Uuid::new_v4(),
            lifecycle_id: Uuid::new_v4(),
            host_version: "2.1.301".into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: base.path().canonicalize().unwrap(),
        };
        let mut proofs = CallProofs::new(locator, "mcp__agentchat__".into()).unwrap();
        proofs
            .register_lifecycle(NativeLifecycle::Bootstrap(lifecycle.clone()))
            .unwrap();
        proofs
            .record(NativeCall {
                observer: lifecycle.observer.clone(),
                generation: lifecycle.generation,
                root: lifecycle.root,
                sequence: 1,
                tool_name: "mcp__agentchat__agentchat_whoami".into(),
                tool_use_id: TOOL_ID.into(),
            })
            .unwrap();
        let backend = Arc::new(crate::process::operations(
            manager.clone(),
            std::env::current_exe().unwrap(),
        ));
        let roots = Arc::new(Conversations::new(backend));
        let diagnostics = Diagnostics::new(None);
        let bridge = Bridge {
            physical_owner: owner.clone(),
            profile_version: lifecycle.host_version.clone(),
            cursor: None,
            transport_nonce: Uuid::new_v4(),
            os_profile: lifecycle.os_profile.clone(),
            native: Mutex::new(Native {
                proofs,
                owner: Some(owner),
                freshness: Freshness::default(),
            }),
            roots: roots.clone(),
            manager,
            directory: base.path().into(),
            diagnostics: diagnostics.clone(),
            restoration: Mutex::default(),
            projection: Mutex::new(Projection::new(&roots, diagnostics)),
        };
        Self {
            base,
            bridge,
            lifecycle,
        }
    }

    fn scope(&self) -> NativeScope {
        NativeScope::Bootstrap {
            lifecycle_id: self.lifecycle.lifecycle_id,
            generation: self.lifecycle.generation,
        }
    }

    fn listen(&self) -> UnixListener {
        let socket = self.base.path().join("relay.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.base.path().join("fresh-observer.json"))
            .unwrap();
        // This socket supplies explicit synthetic observations, never a host
        // compatibility result or an actual native-process receipt.
        file.write_all(
            &serde_json::to_vec(&json!({"observer":"observer","socket":socket,"pid":0})).unwrap(),
        )
        .unwrap();
        listener
    }

    async fn close(&self) {
        assert!(
            !self.base.path().join("state").exists(),
            "Verification alone must not dispatch a backend operation"
        );
        self.bridge.projection.lock().await.close().await;
    }
}

#[tokio::test]
async fn short_native_control_contention_waits_then_verifies_the_original_proof() {
    let f = Fixture::new();
    let listener = f.listen();
    let lifecycle = f.lifecycle.clone();
    let responder = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let challenge = framing::read::<_, Challenge>(&mut BufReader::new(read))
            .await
            .unwrap()
            .unwrap();
        framing::write(
            &mut write,
            &Snapshot {
                nonce: challenge.nonce,
                relay_sequence: 1,
                native_sequence: 1,
                observer: lifecycle.observer,
                owner_root: lifecycle.owner_root,
                root: lifecycle.root,
                generation: lifecycle.generation,
                epoch: 1,
                available: true,
                paused: false,
                pause_cause: None,
                approval: false,
                active_turns: vec![],
                guard_present: true,
                surfaces: vec!["terminal".into()],
                version: lifecycle.host_version,
                unix: 0,
            },
        )
        .await
        .unwrap();
        write.shutdown().await.unwrap();
    });
    let held = f.bridge.native.lock().await;
    let metadata = serde_json::from_value(json!({"claudecode/toolUseId":TOOL_ID})).unwrap();
    let mut verifying = Box::pin(f.bridge.verify_scope(CallEvidence {
        tool_name: "agentchat_whoami",
        metadata: Some(&metadata),
    }));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut verifying)
            .await
            .is_err()
    );
    drop(held);
    assert!(matches!(
        verifying.await.unwrap(),
        CallScope::Bootstrap(key) if key.lifecycle_id == f.lifecycle.lifecycle_id && key.generation == 1
    ));
    responder.await.unwrap();
    assert!(
        f.bridge
            .native
            .lock()
            .await
            .proofs
            .consume_scope("agentchat_whoami", TOOL_ID)
            .is_err(),
        "The successful call consumes the original proof exactly once"
    );
    f.close().await;
}

#[tokio::test]
async fn native_control_deadline_is_retryable_without_spending_proof() {
    let f = Fixture::new();
    let held = f.bridge.native.lock().await;
    let metadata = serde_json::from_value(json!({"claudecode/toolUseId":TOOL_ID})).unwrap();
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(6),
        f.bridge.verify_scope(CallEvidence {
            tool_name: "agentchat_whoami",
            metadata: Some(&metadata),
        }),
    )
    .await
    .expect("The native lock wait must have its own bounded deadline")
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);
    assert!(error.retryable);
    assert!(started.elapsed() >= Duration::from_secs(5));
    let missing = f
        .bridge
        .verify_scope(CallEvidence {
            tool_name: "agentchat_whoami",
            metadata: None,
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::SessionRequired);
    assert!(!missing.retryable);
    drop(held);
    assert_eq!(
        f.bridge
            .native
            .lock()
            .await
            .proofs
            .consume_scope("agentchat_whoami", TOOL_ID)
            .unwrap(),
        f.scope()
    );
    f.close().await;
}

#[tokio::test]
async fn cancelling_a_native_control_wait_does_not_spend_the_call_proof() {
    let f = Fixture::new();
    let held = f.bridge.native.lock().await;
    let metadata = serde_json::from_value(json!({"claudecode/toolUseId":TOOL_ID})).unwrap();
    let mut verifying = Box::pin(f.bridge.verify_scope(CallEvidence {
        tool_name: "agentchat_whoami",
        metadata: Some(&metadata),
    }));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut verifying)
            .await
            .is_err()
    );
    drop(verifying);
    drop(held);
    assert_eq!(
        f.bridge
            .native
            .lock()
            .await
            .proofs
            .consume_scope("agentchat_whoami", TOOL_ID)
            .unwrap(),
        f.scope()
    );
    f.close().await;
}

#[tokio::test]
async fn waiting_calls_recheck_retirement_replacement_and_owner_loss() {
    for change in ["retire", "replace", "owner-loss"] {
        let f = Fixture::new();
        let mut held = f.bridge.native.lock().await;
        let metadata = serde_json::from_value(json!({"claudecode/toolUseId":TOOL_ID})).unwrap();
        let mut verifying = Box::pin(f.bridge.verify_scope(CallEvidence {
            tool_name: "agentchat_whoami",
            metadata: Some(&metadata),
        }));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut verifying)
                .await
                .is_err()
        );
        if change == "owner-loss" {
            held.owner = None;
        } else {
            assert!(
                held.proofs
                    .retire_scope("observer", 1, f.lifecycle.root)
                    .is_some()
            );
            if change == "replace" {
                let mut next = f.lifecycle.clone();
                next.generation = 2;
                next.root = Uuid::new_v4();
                next.lifecycle_id = Uuid::new_v4();
                held.proofs
                    .register_lifecycle(NativeLifecycle::Bootstrap(next))
                    .unwrap();
            }
        }
        drop(held);
        assert_eq!(
            verifying.await.unwrap_err().code,
            ErrorCode::SessionRequired,
            "A waiter must not borrow authority after {change}"
        );
        f.close().await;
    }
}
