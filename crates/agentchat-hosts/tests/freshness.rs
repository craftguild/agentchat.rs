//! Synthetic native frames and actual owned kernel tokens; no host admission.
use agentchat_core::{
    ErrorCode,
    model::{Context, HostKind, Phase},
};
use agentchat_hosts::claude::{
    NativeRegistration,
    freshness::{Challenge, Freshness, Snapshot},
    process::ProcessIdentity,
};
use std::time::Duration;
use uuid::Uuid;

fn setup() -> (Freshness, NativeRegistration, ProcessIdentity) {
    let root = Uuid::new_v4();
    let registration = NativeRegistration {
        owner_root: root,
        observer: "synthetic-observer".into(),
        generation: 1,
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
    };
    (
        Freshness::default(),
        registration,
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
}
fn answer(r: &NativeRegistration, q: Challenge, n: u64) -> Snapshot {
    Snapshot {
        nonce: q.nonce,
        relay_sequence: n,
        native_sequence: n,
        observer: r.observer.clone(),
        owner_root: r.owner_root,
        root: r.root,
        generation: r.generation,
        epoch: 1,
        available: true,
        paused: false,
        pause_cause: None,
        approval: false,
        active_turns: vec![],
        guard_present: true,
        surfaces: vec!["terminal".into()],
        version: r.context.host_version.clone(),
        unix: 0,
    }
}
fn next_root(r: &NativeRegistration) -> NativeRegistration {
    let mut c = r.clone();
    c.generation += 1;
    c.root = Uuid::new_v4();
    c.context.host_session_id = c.root.to_string();
    c.context.context_id = Uuid::new_v4();
    c.context.incarnation = Uuid::new_v4();
    c
}
#[test]
fn fresh_answers_map_idle_active_approval_and_pause_without_clearing_controls() {
    let (mut f, r, p) = setup();
    for (n, paused, approval, active, expected) in [
        (1, false, false, false, Phase::Idle),
        (2, false, false, true, Phase::Active),
        (3, false, true, true, Phase::ApprovalBlocked),
        (4, true, false, false, Phase::Paused),
        (5, true, true, true, Phase::ApprovalBlocked),
    ] {
        let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
        let mut a = answer(&r, q, n);
        a.paused = paused;
        a.approval = approval;
        if active {
            a.active_turns.push("native-turn".into());
        }
        let state = f.finish(&r, &p, a).unwrap();
        assert_eq!(state.phase, expected);
        assert_eq!(state.wake_paused, paused);
        assert_eq!(
            state.active_turn_id.as_deref(),
            active.then_some("native-turn")
        );
    }
}
#[test]
fn optional_pause_diagnostics_never_grant_identity_or_clear_native_controls() {
    for case in 0..4 {
        let (mut f, r, p) = setup();
        let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
        let mut a = answer(&r, q, 1);
        let legacy = serde_json::to_value(&a).unwrap();
        assert!(legacy.get("pause_cause").is_none());
        assert!(
            serde_json::from_value::<Snapshot>(legacy)
                .unwrap()
                .pause_cause
                .is_none()
        );
        a.pause_cause = Some(serde_json::json!({
            "code":"continued", "root":r.root, "paused":false,
            "available":true, "guard_present":true
        }));
        match case {
            0 => a.paused = true,
            1 => a.approval = true,
            2 => a.root = Uuid::new_v4(),
            _ => a.available = false,
        }
        let state = f.finish(&r, &p, a);
        match case {
            0 => {
                let state = state.unwrap();
                assert_eq!(state.phase, Phase::Paused);
                assert!(state.wake_paused);
            }
            1 => assert_eq!(state.unwrap().phase, Phase::ApprovalBlocked),
            _ => assert_eq!(state.unwrap_err().code, ErrorCode::SessionRequired),
        }
    }
}
#[test]
fn mismatched_nonce_cannot_consume_the_matching_request_or_replay_a_success() {
    let (mut f, r, p) = setup();
    let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
    let mut wrong = answer(&r, q, 1);
    wrong.nonce = Uuid::new_v4();
    assert_eq!(
        f.finish(&r, &p, wrong).unwrap_err().code,
        ErrorCode::SessionRequired
    );
    let a = answer(&r, q, 1);
    assert_eq!(f.finish(&r, &p, a.clone()).unwrap().phase, Phase::Idle);
    assert!(f.finish(&r, &p, a).is_err());
}
#[test]
fn one_pending_request_and_exact_cancellation_fence_late_answers() {
    let (mut f, r, p) = setup();
    let old = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
    let busy = f.begin(&r, &p, Duration::from_secs(1)).unwrap_err();
    assert_eq!(busy.code, ErrorCode::Busy);
    assert!(busy.retryable);
    assert!(!f.cancel(Uuid::new_v4()));
    assert!(f.cancel(old.nonce));
    let new = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
    assert!(!f.cancel(old.nonce));
    assert!(f.finish(&r, &p, answer(&r, old, 1)).is_err());
    assert!(f.finish(&r, &p, answer(&r, new, 2)).is_ok());
}
#[test]
fn root_switch_and_changed_descriptor_cannot_complete_an_old_request() {
    let (mut f, r, p) = setup();
    let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
    let c = next_root(&r);
    assert!(f.finish(&c, &p, answer(&r, q, 1)).is_err());
    let q = f.begin(&c, &p, Duration::from_secs(1)).unwrap();
    assert!(f.finish(&c, &p, answer(&c, q, 2)).is_ok());
    for field in 0..3 {
        let q = f.begin(&c, &p, Duration::from_secs(1)).unwrap();
        let mut wrong = c.clone();
        match field {
            0 => wrong.context.incarnation = Uuid::new_v4(),
            1 => wrong.context.project_id = Uuid::new_v4(),
            _ => wrong.context.working_directory = "/other/project".into(),
        };
        assert!(f.finish(&wrong, &p, answer(&c, q, 3)).is_err());
    }
}
#[test]
fn invalid_or_unavailable_matching_answers_are_consumed_and_never_guess_idle() {
    for field in 0..17 {
        let (mut f, r, p) = setup();
        let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
        let mut a = answer(&r, q, 1);
        match field {
            0 => a.root = Uuid::new_v4(),
            1 => a.owner_root = Uuid::new_v4(),
            2 => a.observer = "another".into(),
            3 => a.generation = 2,
            4 => a.version = "unknown".into(),
            5 => a.available = false,
            6 => a.guard_present = false,
            7 => a.native_sequence = 0,
            8 => a.relay_sequence = 0,
            9 => a.epoch = 0,
            10 => a.active_turns = vec!["one".into(), "two".into()],
            11 => a.active_turns = vec!["line\nbreak".into()],
            12 => a.active_turns = vec!["x".repeat(257)],
            13 => a.surfaces.clear(),
            14 => a.surfaces = vec!["unknown".into()],
            15 => a.surfaces = vec!["terminal".into(); 9],
            _ => a.surfaces.push("bad\n".into()),
        };
        assert!(f.finish(&r, &p, a).is_err(), "field {field}");
        assert!(f.finish(&r, &p, answer(&r, q, 2)).is_err());
    }
}
#[test]
fn replay_counter_survives_root_switch_and_only_success_advances_it() {
    let (mut f, r, p) = setup();
    let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
    f.finish(&r, &p, answer(&r, q, 20)).unwrap();
    let c = next_root(&r);
    let q = f.begin(&c, &p, Duration::from_secs(1)).unwrap();
    assert!(f.finish(&c, &p, answer(&c, q, 20)).is_err());
    let q = f.begin(&c, &p, Duration::from_secs(1)).unwrap();
    let mut a = answer(&c, q, 1000);
    a.available = false;
    assert!(f.finish(&c, &p, a).is_err());
    let q = f.begin(&c, &p, Duration::from_secs(1)).unwrap();
    assert!(f.finish(&c, &p, answer(&c, q, 21)).is_ok());
}
#[test]
fn monotonic_expiry_and_not_the_remote_wall_clock_controls_freshness() {
    let (mut f, r, p) = setup();
    let old = f.begin(&r, &p, Duration::from_millis(10)).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
    let mut late = answer(&r, old, 1);
    late.unix = u64::MAX;
    assert!(f.finish(&r, &p, late).is_err());
    assert!(f.finish(&r, &p, answer(&r, q, 2)).is_ok());
    let q = f.begin(&r, &p, Duration::from_millis(10)).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert!(f.finish(&r, &p, answer(&r, q, 3)).is_err());
}
#[test]
fn physical_identity_and_permanent_close_cannot_be_replaced_by_a_new_nonce() {
    let (mut f, r, p) = setup();
    for d in [Duration::ZERO, Duration::from_secs(6)] {
        assert!(f.begin(&r, &p, d).is_err());
    }
    let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
    f.cancel(q.nonce);
    let mut changed = r.clone();
    changed.observer = "replacement".into();
    assert!(f.begin(&changed, &p, Duration::from_secs(1)).is_err());
    changed = r.clone();
    changed.owner_root = Uuid::new_v4();
    assert!(f.begin(&changed, &p, Duration::from_secs(1)).is_err());
    let q = f.begin(&r, &p, Duration::from_secs(1)).unwrap();
    f.close();
    assert!(f.finish(&r, &p, answer(&r, q, 1)).is_err());
    assert!(f.begin(&r, &p, Duration::from_secs(1)).is_err());
}

#[test]
fn a_cached_native_answer_cannot_survive_its_real_kernel_owner() {
    let (mut f, r, _) = setup();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let expected = std::path::Path::new("/bin/sleep").canonicalize().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let owner = loop {
        if let Ok(owner) = ProcessIdentity::capture(child.id())
            && owner.executable().canonicalize().ok().as_ref() == Some(&expected)
            && owner.is_current()
        {
            break owner;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let q = f.begin(&r, &owner, Duration::from_secs(1)).unwrap();
    let cached = answer(&r, q, 1);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(f.finish(&r, &owner, cached).is_err());
}
