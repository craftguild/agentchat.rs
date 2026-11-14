//! Durable callback retry/projection fencing; no native host capability claim.
use agentchat_core::{Error, ErrorCode};
use agentchat_hosts::claude::{
    NativeBootstrap, NativeLifecycle,
    namespace::{OwnerNamespace, Reservation},
    process::ProcessIdentity,
};
use std::{os::unix::fs::PermissionsExt, path::PathBuf};
use uuid::Uuid;
fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
async fn namespace(r: &tempfile::TempDir) -> OwnerNamespace {
    OwnerNamespace::open(
        r.path(),
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .await
    .unwrap()
}
fn lifecycle(ns: &OwnerNamespace, r: &Reservation) -> NativeLifecycle {
    NativeLifecycle::Bootstrap(NativeBootstrap {
        owner_root: ns.id(),
        observer: ns.observer().into(),
        generation: r.generation,
        root: r.root,
        lifecycle_id: Uuid::new_v4(),
        host_version: "2.1.301".into(),
        os_profile: "synthetic".into(),
        working_directory: r.working_directory.clone(),
    })
}
async fn active(ns: &OwnerNamespace) -> Reservation {
    let r = ns
        .reserve(Uuid::new_v4(), PathBuf::from("/"), None)
        .await
        .unwrap();
    ns.commit(r.clone(), lifecycle(ns, &r)).await.unwrap();
    r
}
#[tokio::test]
async fn superseded_or_retired_intent_cannot_publish_while_exact_retry_repairs_partial_projection()
{
    let r = root();
    let ns = namespace(&r).await;
    let a = active(&ns).await;
    let exact = ns.registration(a.clone()).await.unwrap().unwrap();
    let marker = r.path().join("projection");
    let first = marker.clone();
    assert!(
        ns.publish_current(a.clone(), move |_| {
            std::fs::write(first, b"partial").unwrap();
            Err::<(), _>(Error::new(
                ErrorCode::IoError,
                "injected projection interruption",
            ))
        })
        .await
        .is_err()
    );
    assert_eq!(
        ns.registration(a.clone()).await.unwrap(),
        Some(exact.clone())
    );
    let repaired = marker.clone();
    ns.publish_current(a.clone(), move |c| {
        assert_eq!(c.lifecycle, exact);
        std::fs::write(repaired, b"complete").unwrap();
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&marker).unwrap(), b"complete");
    let b = ns.reserve(Uuid::new_v4(), "/".into(), None).await.unwrap();
    assert!(ns.registration(a.clone()).await.is_err());
    let forbidden = marker.clone();
    assert!(
        ns.publish_current(a, move |_| {
            std::fs::write(forbidden, b"wrong").unwrap();
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&marker).unwrap(), b"complete");
    assert!(ns.registration(b.clone()).await.unwrap().is_none());
    ns.retire(b.root, b.generation).await.unwrap();
    assert!(ns.registration(b).await.is_err());
}
#[tokio::test]
async fn repeated_retirement_repairs_cleanup_without_reviving_or_touching_a_successor() {
    let r = root();
    let ns = namespace(&r).await;
    let a = active(&ns).await;
    assert!(
        ns.retire_with(a.root, a.generation, |_| Err(Error::new(
            ErrorCode::IoError,
            "injected cleanup interruption"
        )))
        .await
        .is_err()
    );
    assert!(ns.current().await.unwrap().is_some());
    assert!(ns.retire(a.root, a.generation).await.unwrap());
    let marker = r.path().join("cleanup");
    let repaired = marker.clone();
    assert!(
        !ns.retire_with(a.root, a.generation, move |_| {
            std::fs::write(repaired, b"cleaned").unwrap();
            Ok(())
        })
        .await
        .unwrap()
    );
    assert!(ns.current().await.unwrap().is_none());
    let b = active(&ns).await;
    assert!(
        !ns.retire_with(a.root, a.generation, |_| panic!(
            "late cleanup borrowed successor"
        ))
        .await
        .unwrap()
    );
    assert_eq!(ns.current().await.unwrap().unwrap().reservation, b);
    assert_eq!(std::fs::read(marker).unwrap(), b"cleaned");
}
#[tokio::test]
async fn pending_call_retries_are_stable_bounded_and_keep_floors_after_clear() {
    let r = root();
    let ns = namespace(&r).await;
    let a = active(&ns).await;
    let all = futures_util::future::join_all((0..16).map(|_| {
        ns.prepare_call(
            a.root,
            a.generation,
            "mcp__agentchat__agentchat_status".into(),
            "original".into(),
        )
    }))
    .await;
    let call = all[0].as_ref().unwrap().clone();
    assert_eq!(call.sequence, 1);
    assert!(all.iter().all(|c| c.as_ref().unwrap() == &call));
    let reopened = namespace(&r).await;
    assert_eq!(
        reopened
            .prepare_call(
                a.root,
                a.generation,
                call.tool_name.clone(),
                call.tool_use_id.clone()
            )
            .await
            .unwrap(),
        call
    );
    assert!(
        ns.prepare_call(
            a.root,
            a.generation,
            "mcp__agentchat__agentchat_join".into(),
            "original".into()
        )
        .await
        .is_err()
    );
    for n in 1..32 {
        assert_eq!(
            ns.prepare_call(
                a.root,
                a.generation,
                "mcp__agentchat__agentchat_status".into(),
                format!("call_{n}")
            )
            .await
            .unwrap()
            .sequence,
            n + 1
        );
    }
    let full = ns
        .prepare_call(
            a.root,
            a.generation,
            "mcp__agentchat__agentchat_status".into(),
            "overflow".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(full.code, ErrorCode::Busy);
    assert!(full.retryable);
    assert_eq!(
        ns.prepare_call(
            a.root,
            a.generation,
            call.tool_name.clone(),
            call.tool_use_id.clone()
        )
        .await
        .unwrap(),
        call
    );
    assert!(
        !ns.forget_call(Uuid::new_v4(), a.generation, "original".into())
            .await
            .unwrap()
    );
    assert!(
        ns.forget_call(a.root, a.generation, "original".into())
            .await
            .unwrap()
    );
    assert_eq!(
        ns.prepare_call(
            a.root,
            a.generation,
            "mcp__agentchat__agentchat_status".into(),
            "new_slot".into()
        )
        .await
        .unwrap()
        .sequence,
        33
    );
    let b = ns.reserve(Uuid::new_v4(), "/".into(), None).await.unwrap();
    assert!(
        ns.prepare_call(
            a.root,
            a.generation,
            "mcp__agentchat__agentchat_status".into(),
            "late".into()
        )
        .await
        .is_err()
    );
    ns.commit(b.clone(), lifecycle(&ns, &b)).await.unwrap();
    assert_eq!(
        ns.prepare_call(
            b.root,
            b.generation,
            "mcp__agentchat__agentchat_status".into(),
            "new_root".into()
        )
        .await
        .unwrap()
        .sequence,
        34
    );
    ns.retire(b.root, b.generation).await.unwrap();
    assert!(ns.current().await.unwrap().is_none());
}
#[tokio::test]
async fn invalid_native_calls_do_not_consume_order_or_leave_retry_slots() {
    let r = root();
    let ns = namespace(&r).await;
    let a = active(&ns).await;
    let path = ns.directory().join("owner-state.json");
    let before = std::fs::read(&path).unwrap();
    let oversized = format!("mcp__agentchat__{}", "a".repeat(129));
    for (tool, id) in [
        ("mcp__foreign__agentchat_status", "id"),
        ("mcp__agentchat__", "id"),
        ("mcp__agentchat__agentchat_status", ""),
        ("mcp__agentchat__agentchat_status", "bad:id"),
        (oversized.as_str(), "id"),
    ] {
        assert!(
            ns.prepare_call(a.root, a.generation, tool.into(), id.into())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    assert_eq!(
        ns.prepare_call(
            a.root,
            a.generation,
            "mcp__agentchat__agentchat_status".into(),
            "valid".into()
        )
        .await
        .unwrap()
        .sequence,
        1
    );
    assert_eq!(
        ns.prepare_call(
            a.root,
            a.generation,
            format!("mcp__agentchat__{}", "a".repeat(128)),
            "maximum".into(),
        )
        .await
        .unwrap()
        .sequence,
        2
    );
}

#[tokio::test]
async fn a_delayed_native_reservation_cannot_supersede_a_newer_committed_root() {
    let r = root();
    let ns = namespace(&r).await;
    let old_floor = ns.generation_floor().await.unwrap();
    assert_eq!(old_floor, 0);
    let current = ns
        .reserve_after(old_floor, Uuid::new_v4(), "/".into(), None)
        .await
        .unwrap();
    ns.commit(current.clone(), lifecycle(&ns, &current))
        .await
        .unwrap();
    let before = std::fs::read(ns.directory().join("owner-state.json")).unwrap();
    assert!(
        ns.reserve_after(old_floor, Uuid::new_v4(), "/".into(), None)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(ns.directory().join("owner-state.json")).unwrap(),
        before
    );
    assert_eq!(ns.current().await.unwrap().unwrap().reservation, current);
    let fresh_floor = ns.generation_floor().await.unwrap();
    let successor = ns
        .reserve_after(fresh_floor, Uuid::new_v4(), "/".into(), None)
        .await
        .unwrap();
    assert_eq!(successor.generation, current.generation + 1);
}
