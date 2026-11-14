//! Expired pre-entry claims may retry without repeating an accepted native input.
use super::{Capture, Input, SyntheticPort};
use crate::{host::ManagedHost, support};
use agentchat_core::{
    Error, ErrorCode,
    application::{Application, Mutation},
    delivery::{MessagePort, MessageProgress, NativeDispatch, SUBMISSION_SECONDS},
};
use agentchat_runtime::context::ContextRegistry;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;
use uuid::Uuid;

#[tokio::test]
async fn notice_claim_expiry_before_port_entry_is_not_duplicate_acceptance() {
    retry_after_expiry(true, false).await;
}

#[tokio::test]
async fn body_claim_expiry_before_port_entry_is_not_duplicate_acceptance() {
    retry_after_expiry(false, false).await;
}

#[tokio::test]
#[should_panic(expected = "native FIFO reordered or repeated input")]
async fn repeated_native_entry_is_still_rejected_after_claim_retry() {
    retry_after_expiry(true, true).await;
}

async fn retry_after_expiry(notice: bool, repeat: bool) {
    let f = support::Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let registry = ContextRegistry::new(f.manager.clone());
    let mut contexts = Vec::new();
    for name in ["claim-sender", "claim-receiver"] {
        contexts.push(
            registry
                .issue(support::runtime::seed(&f, name))
                .await
                .unwrap(),
        );
    }
    let host = Arc::new(ManagedHost::new(&contexts));
    let clock = Arc::new(support::runtime::ManualClock::default());
    let app = Application::new(
        f.manager.open(&project).await.unwrap(),
        clock.clone(),
        host.clone(),
    );
    for (index, context) in contexts.iter().enumerate() {
        app.start_launch(context, Some(&format!("claim{index}")))
            .await
            .unwrap();
    }
    let sender = &contexts[0];
    let receiver = &contexts[1];
    let capture = Arc::new(Capture::new());
    let (send, mut receive) = mpsc::channel(4);
    let port = SyntheticPort {
        capture: capture.clone(),
        routes: HashMap::from([(receiver.context_id, send)]),
        generations: host.generations(),
    };
    let command = if notice {
        Mutation::Reserve {
            patterns: vec!["claim-notice/**".into()],
            ttl_seconds: Some(30),
            reason: Some("native claim retry notice".into()),
        }
    } else {
        post("first complete body")
    };
    app.execute(Uuid::new_v4(), sender, command).await.unwrap();
    app.execute(Uuid::new_v4(), sender, post("later complete body"))
        .await
        .unwrap();
    let first = app.prepare_native(receiver).await.unwrap().unwrap();
    assert_eq!(matches!(&first, NativeDispatch::Notice(_)), notice);
    assert_eq!(first.attempt(), 1);
    assert!(
        app.native_is_current(
            receiver,
            first.generation(),
            first.token(),
            first.delivery_id()
        )
        .await
        .unwrap()
    );

    // Exercise the runtime's final pre-port fence, without submitting anything.
    // The expired claim is definitely unaccepted; no unknown result is retried.
    clock.advance(SUBMISSION_SECONDS as i64 + 1);
    assert!(
        !app.native_is_current(
            receiver,
            first.generation(),
            first.token(),
            first.delivery_id()
        )
        .await
        .unwrap()
    );
    assert!(
        app.finish_native(
            receiver,
            &first,
            Ok(MessageProgress::NotAccepted(Error::new(
                ErrorCode::SessionRequired,
                "The committed native input claim expired before port entry.",
            )))
        )
        .await
        .unwrap()
    );
    assert_eq!(capture.data.lock().unwrap().presentations, 0);
    assert!(app.prepare_native(receiver).await.unwrap().is_none());
    clock.advance(SUBMISSION_SECONDS as i64 + 1);
    app.attach(receiver).await.unwrap().unwrap();
    let retry = app.prepare_native(receiver).await.unwrap().unwrap();
    assert_eq!(retry.input(), first.input());
    assert_eq!(retry.attempt(), 2);
    assert_ne!(retry.token(), first.token());
    let outcome = match &retry {
        NativeDispatch::Message(body) => port.present_message(receiver, body).await,
        NativeDispatch::Notice(event) => port.present_notice(receiver, event).await,
    };
    if repeat {
        let NativeDispatch::Notice(event) = &retry else {
            unreachable!();
        };
        port.present_notice(receiver, event).await.unwrap();
    }
    assert!(app.finish_native(receiver, &retry, outcome).await.unwrap());
    if !notice {
        let Input::Body(body) = receive.recv().await.unwrap() else {
            panic!("expected the original complete body");
        };
        assert_eq!(body.body, "first complete body");
    }
    let next = app.prepare_native(receiver).await.unwrap().unwrap();
    let NativeDispatch::Message(body) = &next else {
        panic!("the successor must follow the accepted original head");
    };
    assert_eq!(body.input.body, "later complete body");
    assert!(next.seq() > retry.seq());
    let outcome = port.present_message(receiver, body).await;
    assert!(app.finish_native(receiver, &next, outcome).await.unwrap());
    let Input::Body(body) = receive.recv().await.unwrap() else {
        panic!("expected the complete successor body");
    };
    assert_eq!(body.body, "later complete body");
    assert!(app.prepare_native(receiver).await.unwrap().is_none());
    let expected = 2;
    let status = app
        .health()
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert!(
        status["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    assert_eq!(
        status["statistics"]["read_receipts"],
        if notice { 1 } else { 2 }
    );
    let data = capture.data.lock().unwrap();
    assert_eq!(data.presentations, expected as usize);
    assert_eq!(data.body_deliveries.len(), if notice { 1 } else { 2 });
    assert_eq!(data.notice_deliveries.len(), usize::from(notice));
    assert_eq!(data.max_claim_attempt, 2);
}

fn post(body: &str) -> Mutation {
    Mutation::Post {
        channel: Some("dev".into()),
        body: body.into(),
    }
}
