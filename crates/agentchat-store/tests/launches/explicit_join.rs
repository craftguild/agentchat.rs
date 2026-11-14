//! Explicit activation uses one original owner across fresh participation lives.
use super::*;

#[tokio::test]
async fn live_original_owner_rejoins_fresh_without_inactive_catchup_or_replay_activation() {
    let f = Fixture::new().await;
    let sender = f.context();
    f.join(&sender, "sender").await;
    let context = f.context();
    let gate = LaunchGate::new(&f);
    let original = gate.bind(&context);
    let app = gate.app(&f);
    let join = Mutation::Join {
        name: Some("receiver".into()),
    };
    let request = Uuid::new_v4();
    let first = app.execute(request, &context, join.clone()).await.unwrap();
    let mut before = Vec::new();
    for table in [
        "actors",
        "sessions",
        "launches",
        "events",
        "message_streams",
    ] {
        before.push(f.count(table).await);
    }
    let duplicate = app
        .execute(Uuid::new_v4(), &context, join.clone())
        .await
        .unwrap();
    assert_eq!(duplicate.result, first.result);
    assert!(duplicate.event_head.is_none());
    // Only an idempotency receipt is added by the independent duplicate request.
    for (i, table) in [
        "actors",
        "sessions",
        "launches",
        "events",
        "message_streams",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(before[i], f.count(table).await, "{table}");
    }
    execute(&app, &context, Mutation::Leave).await;
    let inactive = id(
        &f.post(&sender, "@receiver while left").await.result,
        "message_id",
    );
    let left_counts = f.counts().await;
    assert_eq!(
        app.execute(request, &context, join.clone())
            .await
            .unwrap()
            .result,
        first.result
    );
    assert_eq!(left_counts, f.counts().await);
    assert!(app.attach(&context).await.is_err());

    let mut fresh = context.clone();
    fresh.context_id = Uuid::new_v4();
    fresh.incarnation = Uuid::new_v4();
    let successor = LaunchIdentity {
        generation: Uuid::new_v4(),
        owner: original.owner.clone(),
    };
    gate.bindings
        .lock()
        .unwrap()
        .insert(fresh.context_id, successor.clone());
    gate.state(&successor, LaunchOwnerState::Current);
    let rejoined = app
        .execute(Uuid::new_v4(), &fresh, Mutation::Join { name: None })
        .await
        .unwrap();
    assert_eq!(rejoined.result["agent_id"], first.result["agent_id"]);
    assert_ne!(
        rejoined.result["join_boundary"],
        first.result["join_boundary"]
    );
    assert_eq!(f.count("launches").await, 2);
    assert!(app.heartbeat(&context).await.is_err());
    let message = id(
        &f.post(&sender, "@receiver after rejoin").await.result,
        "message_id",
    );
    let dispatch = app.prepare_wake(&fresh).await.unwrap().unwrap();
    let selected = dispatch
        .hint
        .events
        .iter()
        .map(|e| e.message_id)
        .collect::<Vec<_>>();
    assert!(selected.contains(&message));
    assert!(!selected.contains(&inactive));
    let mut tx = f.store.begin().await.unwrap();
    let old = tx.launch(original.generation).await.unwrap().unwrap();
    let new = tx.launch(successor.generation).await.unwrap().unwrap();
    assert_eq!(old.retirement_reason.as_deref(), Some("explicit_leave"));
    assert_eq!(new.identity.owner, old.identity.owner);
    assert_eq!(new.context_id, fresh.context_id);
    assert!(new.retired_at.is_none());
}

#[tokio::test]
async fn gone_or_unconfirmed_original_owner_never_activates_or_reactivates() {
    let f = Fixture::new().await;
    let context = f.context();
    let gate = LaunchGate::new(&f);
    let identity = gate.bind(&context);
    let app = gate.app(&f);
    for state in [LaunchOwnerState::Unknown, LaunchOwnerState::Gone] {
        gate.state(&identity, state);
        let before = f.counts().await;
        let error = app
            .execute(Uuid::new_v4(), &context, Mutation::Join { name: None })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SessionRequired);
        assert!(!error.retryable);
        assert_eq!(error.details["reason"], "managed_launch_required");
        assert_eq!(before, f.counts().await);
    }
}

#[tokio::test]
async fn launch_name_is_only_a_first_join_default_and_does_not_change_request_hashing() {
    let f = Fixture::new().await;
    let context = f.context();
    let gate = LaunchGate::new(&f);
    let original = gate.bind(&context);
    *gate.default_name.lock().unwrap() = Some("launch-default".into());
    let app = gate.app(&f);
    let request = Uuid::new_v4();
    let command = Mutation::Join { name: None };
    let first = app
        .execute(request, &context, command.clone())
        .await
        .unwrap();
    assert_eq!(first.result["handle"], "launch-default");
    assert_eq!(
        app.execute(request, &context, command.clone())
            .await
            .unwrap()
            .result,
        first.result
    );
    execute(&app, &context, Mutation::Leave).await;
    let mut fresh = context.clone();
    fresh.context_id = Uuid::new_v4();
    fresh.incarnation = Uuid::new_v4();
    let successor = LaunchIdentity {
        generation: Uuid::new_v4(),
        owner: original.owner,
    };
    gate.bindings
        .lock()
        .unwrap()
        .insert(fresh.context_id, successor.clone());
    gate.state(&successor, LaunchOwnerState::Current);
    *gate.default_name.lock().unwrap() = Some("another-launch-name".into());
    let rejoined = app.execute(Uuid::new_v4(), &fresh, command).await.unwrap();
    assert_eq!(rejoined.result["handle"], "launch-default");
}

#[tokio::test]
async fn a_cached_join_receipt_does_not_bypass_original_owner_loss() {
    let f = Fixture::new().await;
    let context = f.context();
    let gate = LaunchGate::new(&f);
    let identity = gate.bind(&context);
    let app = gate.app(&f);
    let request = Uuid::new_v4();
    let command = Mutation::Join { name: None };
    let first = app
        .execute(request, &context, command.clone())
        .await
        .unwrap();
    let before = f.counts().await;
    for state in [LaunchOwnerState::Unknown, LaunchOwnerState::Gone] {
        gate.state(&identity, state);
        let error = app
            .execute(request, &context, command.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SessionRequired);
        assert_eq!(error.details["reason"], "managed_launch_required");
        assert_eq!(before, f.counts().await);
    }
    gate.state(&identity, LaunchOwnerState::Current);
    assert_eq!(
        app.execute(request, &context, command)
            .await
            .unwrap()
            .result,
        first.result
    );
}

#[tokio::test]
async fn single_and_grouped_mutations_observe_native_state_before_acquiring_the_full_writer() {
    for grouped in [false, true] {
        let f = Fixture::new().await;
        let context = f.context();
        let gate = LaunchGate::new(&f);
        gate.bind(&context);
        *gate.writer_probe.lock().unwrap() = Some(f.store.clone());
        let app = gate.app(&f);
        let command = Mutation::Join {
            name: Some("writer-free-observation".into()),
        };
        let request = Uuid::new_v4();
        if grouped {
            let outcomes = app
                .execute_group(vec![agentchat_core::application::MutationRequest {
                    request_id: request,
                    context: context.clone(),
                    command,
                }])
                .await
                .unwrap();
            assert_eq!(
                outcomes.into_iter().next().unwrap().unwrap().result["participation"],
                "joined"
            );
        } else {
            assert_eq!(
                app.execute(request, &context, command)
                    .await
                    .unwrap()
                    .result["participation"],
                "joined"
            );
        }
        let post = Mutation::Post {
            channel: Some("dev".into()),
            body: "Verified outside the writer".into(),
        };
        if grouped {
            app.execute_group(vec![agentchat_core::application::MutationRequest {
                request_id: Uuid::new_v4(),
                context: context.clone(),
                command: post,
            }])
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .unwrap();
        } else {
            app.execute(Uuid::new_v4(), &context, post).await.unwrap();
        }
        assert_eq!(f.count("launches").await, 1);
        assert_eq!(f.count("dispatch_messages").await, 1);
    }
}
