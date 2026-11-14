//! Synthetic native facts exercise the real ledger/challenge, not host admission.
use agentchat_core::model::{Context, HostKind, Phase};
use agentchat_hosts::claude::{
    CallProofs, NativeBootstrap, NativeCall, NativeLifecycle, NativeRegistration, NativeScope,
    freshness::{Challenge, Freshness, Snapshot},
    process::ProcessIdentity,
};
use std::time::Duration;
use uuid::Uuid;

fn bootstrap() -> NativeBootstrap {
    NativeBootstrap {
        owner_root: Uuid::new_v4(),
        observer: "owned-observer".into(),
        generation: 1,
        root: Uuid::new_v4(),
        lifecycle_id: Uuid::new_v4(),
        host_version: "2.1.301".into(),
        os_profile: "synthetic".into(),
        working_directory: "/owned/uninitialized".into(),
    }
}
fn project(b: &NativeBootstrap) -> NativeRegistration {
    NativeRegistration {
        owner_root: b.owner_root,
        observer: b.observer.clone(),
        generation: b.generation + 1,
        root: b.root,
        context: Context {
            context_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            host: HostKind::Claude,
            host_session_id: b.root.to_string(),
            host_version: b.host_version.clone(),
            os_profile: b.os_profile.clone(),
            working_directory: b.working_directory.clone(),
        },
    }
}
fn call(r: &NativeLifecycle, n: u64) -> NativeCall {
    NativeCall {
        observer: r.observer().into(),
        generation: r.generation(),
        root: r.root(),
        sequence: n,
        tool_name: "mcp__agentchat__agentchat_init".into(),
        tool_use_id: format!("toolu_{n}"),
    }
}
fn answer(r: &NativeLifecycle, q: Challenge, n: u64) -> Snapshot {
    Snapshot {
        nonce: q.nonce,
        relay_sequence: n,
        native_sequence: n,
        observer: r.observer().into(),
        owner_root: r.owner_root(),
        root: r.root(),
        generation: r.generation(),
        epoch: 1,
        available: true,
        paused: true,
        pause_cause: None,
        approval: true,
        active_turns: vec![],
        guard_present: true,
        surfaces: vec!["terminal".into()],
        version: r.host_version().into(),
        unix: 0,
    }
}

#[test]
fn bootstrap_requires_explicit_native_registration_and_never_returns_a_context() {
    let b = bootstrap();
    let r = NativeLifecycle::Bootstrap(b.clone());
    let mut p = CallProofs::new(b.owner_root, "mcp__agentchat__".into()).unwrap();
    assert!(p.record(call(&r, 1)).is_err());
    assert!(p.consume_scope("agentchat_init", "toolu_1").is_err());
    assert!(p.register_bootstrap(b.clone()).unwrap());
    assert!(!p.register_bootstrap(b.clone()).unwrap());
    assert!(p.current().is_none());
    assert_eq!(p.lifecycle(), Some(&r));
    p.record(call(&r, 1)).unwrap();
    assert_eq!(
        p.consume_scope("agentchat_init", "toolu_1").unwrap(),
        r.scope()
    );
    assert_eq!(
        r.scope(),
        NativeScope::Bootstrap {
            lifecycle_id: b.lifecycle_id,
            generation: b.generation,
        }
    );
    assert!(p.consume_scope("agentchat_init", "toolu_1").is_err());
    assert!(p.record(call(&r, 1)).is_err());
    p.record(call(&r, 2)).unwrap();
    assert!(p.consume("agentchat_init", "toolu_2").is_err());
    assert!(p.consume_scope("agentchat_init", "toolu_2").is_err());
    p.record(call(&r, 3)).unwrap();
    assert!(p.consume_scope("agentchat_post", "toolu_3").is_err());
    assert!(p.consume_scope("agentchat_init", "toolu_3").is_err());
    let wire = serde_json::to_value(&r).unwrap();
    assert!(wire["registration"].get("context").is_none());
    assert_eq!(
        serde_json::from_value::<NativeLifecycle>(wire.clone()).unwrap(),
        r
    );
    let mut wrong = wire.clone();
    wrong["context"] = serde_json::json!({});
    assert!(serde_json::from_value::<NativeLifecycle>(wrong).is_err());
    let mut wrong = wire;
    wrong["registration"]["context"] = serde_json::json!({});
    assert!(serde_json::from_value::<NativeLifecycle>(wrong).is_err());
}

#[test]
fn malformed_or_conflicting_bootstrap_cannot_replace_native_authority() {
    let b = bootstrap();
    let r = NativeLifecycle::Bootstrap(b.clone());
    let mut p = CallProofs::new(b.owner_root, "mcp__agentchat__".into()).unwrap();
    p.register_bootstrap(b.clone()).unwrap();
    for field in 0..11 {
        let mut bad = b.clone();
        bad.generation += 1;
        bad.lifecycle_id = Uuid::new_v4();
        match field {
            0 => bad.owner_root = Uuid::new_v4(),
            1 => bad.observer = "other-observer".into(),
            2 => bad.generation = 0,
            3 => bad.lifecycle_id = Uuid::nil(),
            4 => bad.lifecycle_id = b.lifecycle_id,
            5 => bad.host_version.clear(),
            6 => bad.host_version = "x".repeat(129),
            7 => bad.os_profile.clear(),
            8 => bad.os_profile = "x".repeat(129),
            9 => bad.working_directory = "relative".into(),
            _ => bad.working_directory = ("/".to_owned() + &"x".repeat(16_384)).into(),
        }
        assert!(p.register_bootstrap(bad).is_err(), "field {field}");
        assert_eq!(p.lifecycle(), Some(&r));
    }
    let mut changed = b.clone();
    changed.root = Uuid::new_v4();
    assert!(p.register_bootstrap(changed).is_err());
}

#[test]
fn promotion_shares_replay_and_retirement_fences_without_lending_scope() {
    let b = bootstrap();
    let old = NativeLifecycle::Bootstrap(b.clone());
    let mut p = CallProofs::new(b.owner_root, "mcp__agentchat__".into()).unwrap();
    p.register_bootstrap(b.clone()).unwrap();
    p.record(call(&old, 1)).unwrap();
    assert_eq!(
        p.retire_scope(&b.observer, b.generation, b.root),
        Some(old.scope())
    );
    assert!(p.consume_scope("agentchat_init", "toolu_1").is_err());
    assert!(p.register_bootstrap(b.clone()).is_err());
    let mut bad = project(&b);
    bad.context.context_id = b.lifecycle_id;
    assert!(p.register(bad).is_err());
    let next = project(&b);
    let new = NativeLifecycle::Project(next.clone());
    p.register(next.clone()).unwrap();
    assert_eq!(p.current(), Some(&next));
    assert!(p.record(call(&new, 1)).is_err());
    assert!(p.record(call(&old, 2)).is_err());
    p.record(call(&new, 2)).unwrap();
    assert!(p.retire_scope(&b.observer, b.generation, b.root).is_none());
    assert!(!p.revoke(&b.observer, b.generation, b.root, "toolu_2"));
    assert_eq!(
        p.consume_scope("agentchat_init", "toolu_2").unwrap(),
        new.scope()
    );
    let mut clear = b.clone();
    clear.generation = 3;
    clear.lifecycle_id = Uuid::new_v4();
    clear.root = Uuid::new_v4();
    p.register_bootstrap(clear.clone()).unwrap();
    assert!(p.current().is_none());
    assert!(p.register(next).is_err());
    p.close();
    assert!(p.lifecycle().is_none());
    clear.generation += 1;
    clear.lifecycle_id = Uuid::new_v4();
    assert!(p.register_bootstrap(clear).is_err());
}

#[test]
fn freshness_requires_the_same_native_bootstrap_then_fences_its_promotion() {
    let b = bootstrap();
    let r = NativeLifecycle::Bootstrap(b.clone());
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut f = Freshness::default();
    let q = f
        .begin_lifecycle(&r, &owner, Duration::from_secs(1))
        .unwrap();
    let state = f.finish_lifecycle(&r, &owner, answer(&r, q, 10)).unwrap();
    assert_eq!(state.phase, Phase::ApprovalBlocked);
    assert!(state.wake_paused);
    let q = f
        .begin_lifecycle(&r, &owner, Duration::from_secs(1))
        .unwrap();
    let mut changed = b.clone();
    changed.lifecycle_id = Uuid::new_v4();
    assert!(
        f.finish_lifecycle(
            &NativeLifecycle::Bootstrap(changed),
            &owner,
            answer(&r, q, 11)
        )
        .is_err()
    );
    let q = f
        .begin_lifecycle(&r, &owner, Duration::from_secs(1))
        .unwrap();
    let next = project(&b);
    let promoted = NativeLifecycle::Project(next.clone());
    assert!(
        f.finish_lifecycle(&promoted, &owner, answer(&r, q, 11))
            .is_err()
    );
    let q = f.begin(&next, &owner, Duration::from_secs(1)).unwrap();
    assert!(f.finish(&next, &owner, answer(&promoted, q, 10)).is_err());
    let q = f.begin(&next, &owner, Duration::from_secs(1)).unwrap();
    assert!(f.finish(&next, &owner, answer(&promoted, q, 11)).is_ok());
    let mut wrong = b;
    wrong.observer = "other-observer".into();
    assert!(
        f.begin_lifecycle(
            &NativeLifecycle::Bootstrap(wrong),
            &owner,
            Duration::from_secs(1)
        )
        .is_err()
    );
}
