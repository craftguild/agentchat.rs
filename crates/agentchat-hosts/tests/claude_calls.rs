//! Native correlation policy only; these fixtures establish no real host liveness.
use agentchat_core::{
    ErrorCode,
    model::{Context, HostKind},
};
use agentchat_hosts::claude::{CallProofs, NativeCall, NativeRegistration};
use uuid::Uuid;

fn registration(owner_root: Uuid, observer: &str, generation: u64) -> NativeRegistration {
    let root = Uuid::new_v4();
    NativeRegistration {
        owner_root,
        observer: observer.into(),
        generation,
        root,
        context: Context {
            context_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            host: HostKind::Claude,
            host_session_id: root.to_string(),
            host_version: "2.1.301".into(),
            os_profile: "synthetic".into(),
            working_directory: "/owned/project".into(),
        },
    }
}
fn call(registration: &NativeRegistration, sequence: u64) -> NativeCall {
    NativeCall {
        observer: registration.observer.clone(),
        generation: registration.generation,
        root: registration.root,
        sequence,
        tool_name: "mcp__agentchat__agentchat_status".into(),
        tool_use_id: format!("toolu_{sequence}"),
    }
}
fn ready() -> (CallProofs, NativeRegistration) {
    let owner = Uuid::new_v4();
    let registration = registration(owner, "observer-a", 1);
    let mut proofs = CallProofs::new(owner, "mcp__agentchat__".into()).unwrap();
    assert!(proofs.register(registration.clone()).unwrap());
    (proofs, registration)
}
#[test]
fn a_spawn_hint_alone_authorizes_nothing_and_native_proof_is_consumed_once() {
    let owner = Uuid::new_v4();
    let mut proofs = CallProofs::new(owner, "mcp__agentchat__".into()).unwrap();
    assert_eq!(
        proofs
            .consume("agentchat_status", "toolu_1")
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    let registration = registration(owner, "observer-a", 1);
    let native = call(&registration, 1);
    assert_eq!(
        proofs.record(native.clone()).unwrap_err().code,
        ErrorCode::SessionRequired
    );
    proofs.register(registration.clone()).unwrap();
    assert!(proofs.record(native.clone()).unwrap());
    assert!(!proofs.record(native.clone()).unwrap());
    assert_eq!(
        proofs.consume("agentchat_status", "toolu_1").unwrap(),
        registration.scope()
    );
    assert_eq!(
        proofs
            .consume("agentchat_status", "toolu_1")
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        proofs.record(native).unwrap_err().code,
        ErrorCode::SessionRequired
    );
}
#[test]
fn invalid_registration_and_ambiguous_owner_never_replace_current_facts() {
    let (mut proofs, current) = ready();
    for field in 0..7 {
        let mut next = registration(current.owner_root, "observer-a", 2);
        match field {
            0 => next.owner_root = Uuid::new_v4(),
            1 => next.observer = "observer-b".into(),
            2 => next.context.host = HostKind::Codex,
            3 => next.context.host_session_id = Uuid::new_v4().to_string(),
            4 => next.context.working_directory = "relative".into(),
            5 => next.context.context_id = current.context.context_id,
            _ => next.context.incarnation = current.context.incarnation,
        }
        assert_eq!(
            proofs.register(next).unwrap_err().code,
            ErrorCode::SessionRequired
        );
        assert_eq!(proofs.current(), Some(&current));
    }
    assert!(!proofs.register(current.clone()).unwrap());
    let mut conflict = current.clone();
    conflict.root = Uuid::new_v4();
    assert!(proofs.register(conflict).is_err());
}
#[test]
fn exact_tool_namespace_and_root_are_required_without_consumption_fallback() {
    let (mut proofs, registration) = ready();
    for field in 0..5 {
        let mut native = call(&registration, field + 1);
        match field {
            0 => native.tool_name = "mcp__foreign__agentchat_status".into(),
            1 => native.root = Uuid::new_v4(),
            2 => native.generation = 2,
            3 => native.observer = "other".into(),
            _ => native.tool_use_id = "x".repeat(257),
        }
        assert!(proofs.record(native).is_err());
    }
    proofs.record(call(&registration, 10)).unwrap();
    assert!(proofs.consume("agentchat_post", "toolu_10").is_err());
    assert!(proofs.consume("agentchat_status", "toolu_10").is_err());
    assert!(proofs.record(call(&registration, 10)).is_err());
}
#[test]
fn clear_retires_pending_proof_without_resetting_the_observation_replay_window() {
    let (mut proofs, a) = ready();
    let old = call(&a, 1);
    proofs.record(old.clone()).unwrap();
    let c = registration(a.owner_root, &a.observer, 2);
    proofs.register(c.clone()).unwrap();
    assert!(proofs.consume("agentchat_status", "toolu_1").is_err());
    assert!(proofs.record(old).is_err());
    assert!(proofs.record(call(&c, 1)).is_err());
    proofs.record(call(&c, 2)).unwrap();
    assert_eq!(
        proofs.consume("agentchat_status", "toolu_2").unwrap(),
        c.scope()
    );
    assert!(proofs.register(a).is_err());
}
#[test]
fn late_end_and_revocation_cannot_touch_a_replacement_root() {
    let (mut proofs, a) = ready();
    let c = registration(a.owner_root, &a.observer, 2);
    proofs.register(c.clone()).unwrap();
    proofs.record(call(&c, 2)).unwrap();
    assert!(proofs.retire(&a.observer, a.generation, a.root).is_none());
    assert!(!proofs.revoke(&a.observer, a.generation, a.root, "toolu_2"));
    assert_eq!(
        proofs.consume("agentchat_status", "toolu_2").unwrap(),
        c.scope()
    );
    proofs.record(call(&c, 3)).unwrap();
    assert!(proofs.revoke(&c.observer, c.generation, c.root, "toolu_3"));
    assert!(proofs.consume("agentchat_status", "toolu_3").is_err());
    assert!(proofs.record(call(&c, 3)).is_err());
    assert_eq!(
        proofs.retire(&c.observer, c.generation, c.root),
        Some(c.scope())
    );
    assert!(proofs.record(call(&c, 4)).is_err());
    assert!(proofs.register(c.clone()).is_err());
    let resumed = registration(c.owner_root, &c.observer, 3);
    proofs.register(resumed).unwrap();
}
#[test]
fn capacity_rejection_has_no_partial_observation_and_reordered_calls_remain_valid() {
    let (mut proofs, r) = ready();
    for sequence in (1..=32).rev() {
        proofs.record(call(&r, sequence)).unwrap();
    }
    let error = proofs.record(call(&r, 33)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);
    assert!(error.retryable);
    proofs.consume("agentchat_status", "toolu_1").unwrap();
    assert!(proofs.record(call(&r, 33)).unwrap());
    for sequence in 2..=33 {
        assert_eq!(
            proofs
                .consume("agentchat_status", &format!("toolu_{sequence}"))
                .unwrap(),
            r.scope()
        );
    }
}
#[test]
fn the_replay_window_stays_bounded_and_rejects_older_observations_after_consumption() {
    let (mut proofs, r) = ready();
    for sequence in 1..=5000 {
        proofs.record(call(&r, sequence)).unwrap();
        proofs
            .consume("agentchat_status", &format!("toolu_{sequence}"))
            .unwrap();
    }
    assert!(proofs.record(call(&r, 1)).is_err());
    assert!(proofs.record(call(&r, 5000)).is_err());
    let mut duplicate = call(&r, 5001);
    duplicate.tool_use_id = "toolu_5000".into();
    assert!(proofs.record(duplicate).is_err());
    assert!(proofs.record(call(&r, 5001)).unwrap());
}
#[test]
fn closing_a_physical_owner_is_irreversible() {
    let (mut proofs, r) = ready();
    proofs.record(call(&r, 1)).unwrap();
    proofs.close();
    assert!(proofs.current().is_none());
    assert!(proofs.consume("agentchat_status", "toolu_1").is_err());
    assert!(
        proofs
            .register(registration(r.owner_root, &r.observer, 2))
            .is_err()
    );
}
