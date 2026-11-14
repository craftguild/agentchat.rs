//! R25/T23: identical shared operations with independent labels and mechanisms.
#[path = "adapter_contract/controls.rs"]
mod controls;
#[path = "adapter_contract/fixture.rs"]
mod fixture;
#[path = "adapter_contract/recovery.rs"]
mod recovery;
mod support;
use agentchat_core::{
    Error, ErrorCode,
    application::Mutation,
    delivery::{InputRecovery, MessageProgress, NativeInput},
    model::{DeliveryState, HostKind, Phase},
    ports::Storage,
};
use fixture::*;

async fn configured_harness<M: Mechanism + 'static>(label: HostKind) -> Harness<M> {
    let harness = Harness::<M>::new(label).await;
    harness
        .app
        .initialize_with_settings(
            None,
            agentchat_core::project::ReceiptSettingsUpdate {
                auto_read: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    harness
}
use uuid::Uuid;

async fn unchanged_fifo<M: Mechanism + 'static>(label: HostKind) {
    let h = configured_harness::<M>(label).await;
    let mut maximum = "@receiver assignment\n\n@sender thanks\n\n日本語 `@ignored`\n".to_owned();
    maximum.push_str(&"x".repeat(65_536 - maximum.len() - 3));
    maximum.push_str("\n \t");
    assert_eq!(maximum.len(), 65_536);
    let first = h.send(&maximum).await;
    let mut passive = "passive unchanged 日本語 context\n\n".to_owned();
    passive.push_str(&"y".repeat(65_536 - passive.len() - 3));
    passive.push_str("\n \t");
    let second = h.send(&passive).await;
    let original = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    let NativeInput::Message(body) = original.input() else {
        panic!("message head")
    };
    assert_eq!(body.body.as_bytes(), maximum.trim().as_bytes());
    assert_eq!(body.message_id, first);
    assert!(body.addressed);
    assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
    assert!(
        h.app
            .finish_native(&h.receiver, &original, h.dispatch(&original).await)
            .await
            .unwrap()
    );
    let mut transaction = h.f.store.begin().await.unwrap();
    assert_eq!(
        transaction
            .delivery(original.delivery_id())
            .await
            .unwrap()
            .unwrap()
            .state,
        DeliveryState::Delivered
    );
    transaction.rollback().await.unwrap();
    h.f.assert_read_kinds(1, 0).await;
    let next = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    let NativeInput::Message(body) = next.input() else {
        panic!("message successor")
    };
    assert_eq!(body.message_id, second);
    assert_eq!(body.body.as_bytes(), passive.trim().as_bytes());
    assert!(!body.addressed);
    assert!(next.seq() > original.seq());
    h.app
        .finish_native(&h.receiver, &next, h.dispatch(&next).await)
        .await
        .unwrap();
    h.f.app
        .execute(
            Uuid::new_v4(),
            &h.sender,
            Mutation::Read {
                message_ids: vec![first],
            },
        )
        .await
        .unwrap();
    h.f.assert_read_kinds(2, 1).await;
    // Explicit reads emit no positive event. Use an ordinary reservation to
    // exercise mixed message/notice FIFO without manufacturing a read notice.
    h.f.app
        .execute(
            Uuid::new_v4(),
            &h.sender,
            Mutation::Reserve {
                patterns: vec!["src/fifo/**".into()],
                ttl_seconds: None,
                reason: None,
            },
        )
        .await
        .unwrap();
    let notice = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    assert!(matches!(notice.input(), NativeInput::Notice(_)));
    assert!(notice.seq() > next.seq());
    h.app
        .finish_native(&h.receiver, &notice, h.dispatch(&notice).await)
        .await
        .unwrap();
    assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
    assert_eq!(h.adapter.calls.lock().unwrap().len(), 3);
    h.f.assert_read_kinds(2, 1).await;
}

#[tokio::test]
async fn three_mechanisms_preserve_maximum_mixed_fifo_independently_of_both_labels() {
    for label in [HostKind::Codex, HostKind::Claude] {
        unchanged_fifo::<Direct>(label).await;
        unchanged_fifo::<Journal>(label).await;
        unchanged_fifo::<TurnBoundJournal>(label).await;
    }
}

async fn unresolved<M: Mechanism + 'static>(label: HostKind) {
    let h = configured_harness::<M>(label).await;
    h.send("@receiver original uncertain input").await;
    let dispatch = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    let before = h.stream().await;
    let recovery = h.adapter.mechanism.capabilities().recovery;
    h.adapter.capabilities.lock().unwrap().recovery = match recovery {
        InputRecovery::OriginalReceipt => InputRecovery::RetainUncertainty,
        InputRecovery::RetainUncertainty => InputRecovery::OriginalReceipt,
    };
    h.f.clock.advance(6);
    for native in [true, false] {
        let code = if native {
            h.app.prepare_native(&h.receiver).await.unwrap_err().code
        } else {
            h.app.prepare_message(&h.receiver).await.unwrap_err().code
        };
        assert_eq!(code, ErrorCode::UnsupportedProtocol);
        assert_eq!(h.stream().await, before);
    }
    assert_eq!(h.adapter.calls.lock().unwrap().len(), 0);
    // Only the original definite result may settle; a declaration is no receipt.
    h.app
        .finish_native(
            &h.receiver,
            &dispatch,
            Ok(MessageProgress::NotAccepted(Error::new(
                ErrorCode::IoError,
                "proved non-entry",
            ))),
        )
        .await
        .unwrap();
    h.f.clock.advance(6);
    let next = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    assert_eq!(next.delivery_id(), dispatch.delivery_id());
    assert_ne!(next.token(), dispatch.token());
    assert_eq!(next.attempt(), 2);
}

#[tokio::test]
async fn changed_mechanism_cannot_reinterpret_an_unresolved_head_on_either_operation() {
    for label in [HostKind::Codex, HostKind::Claude] {
        unresolved::<Direct>(label).await;
        unresolved::<Journal>(label).await;
        unresolved::<TurnBoundJournal>(label).await;
    }
}

#[tokio::test]
async fn third_adapter_has_its_own_active_input_requirement() {
    let h = configured_harness::<TurnBoundJournal>(HostKind::Codex).await;
    h.send("@receiver waiting for exact active turn").await;
    h.adapter.state.lock().unwrap().phase = Phase::Active;
    let before = h.stream().await;
    assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
    assert_eq!(h.stream().await, before);
    h.adapter.state.lock().unwrap().active_turn_id = Some("test-only-turn".into());
    assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_some());
}

async fn attention<M: Mechanism + 'static>(label: HostKind) {
    let h = configured_harness::<M>(label).await;
    let mut contexts = vec![h.receiver.clone()];
    for handle in ["beta", "observer"] {
        let mut context = h.f.context();
        context.host = label;
        h.adapter.bind(&context);
        h.app.start_launch(&context, Some(handle)).await.unwrap();
        contexts.push(context);
    }
    // Consume only pre-existing join notices before the authored body.
    for context in &contexts {
        while let Some(dispatch) = h.app.prepare_native(context).await.unwrap() {
            assert!(matches!(dispatch.input(), NativeInput::Notice(_)));
            h.app
                .finish_native(context, &dispatch, h.dispatch_in(context, &dispatch).await)
                .await
                .unwrap();
        }
    }
    let body = "@receiver please implement the assigned change.\n\n@beta thank you for the earlier review.\n\n  unchanged 日本語 context\n \t";
    let message = h.send(body).await;
    for (index, context) in contexts.iter().enumerate() {
        let dispatch = h.app.prepare_native(context).await.unwrap().unwrap();
        let NativeInput::Message(input) = dispatch.input() else {
            panic!("body")
        };
        assert_eq!(input.message_id, message);
        assert_eq!(input.body.as_bytes(), body.trim().as_bytes());
        assert_eq!(input.addressed, index < 2);
        h.app
            .finish_native(context, &dispatch, h.dispatch_in(context, &dispatch).await)
            .await
            .unwrap();
    }
    h.f.assert_read_kinds(3, 0).await;
    assert_eq!(h.f.count("mentions").await, 2);
}

#[tokio::test]
async fn inline_assignment_and_thanks_preserve_full_audience_without_inferred_acceptance() {
    for label in [HostKind::Codex, HostKind::Claude] {
        attention::<Direct>(label).await;
        attention::<Journal>(label).await;
        attention::<TurnBoundJournal>(label).await;
    }
}
