//! Generation retirement preserves history while cancelling only delivery work.
mod support;

use agentchat_core::{
    ErrorCode, Result,
    application::{Application, ManagedActivationCause, ManagedActivationRequest, Mutation},
    delivery::HostReceipt,
    model::{Context, LaunchIdentity, LaunchOwnerState},
    ports::{HostGate, HostState, Storage},
    read::{PageRequest, Query},
};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use support::{Fixture, id};
use uuid::Uuid;

pub(crate) struct LaunchGate {
    host: Arc<support::SyntheticHost>,
    bindings: Mutex<HashMap<Uuid, LaunchIdentity>>,
    owners: Mutex<HashMap<Uuid, LaunchOwnerState>>,
    default_name: Mutex<Option<String>>,
    writer_probe: Mutex<Option<Arc<dyn Storage>>>,
    fenced: Mutex<HashSet<Uuid>>,
    changed: Mutex<HashSet<Uuid>>,
}
impl LaunchGate {
    pub(crate) fn new(f: &Fixture) -> Arc<Self> {
        Arc::new(Self {
            host: f.host.clone(),
            bindings: Mutex::new(HashMap::new()),
            owners: Mutex::new(HashMap::new()),
            default_name: Mutex::new(None),
            writer_probe: Mutex::new(None),
            fenced: Mutex::new(HashSet::new()),
            changed: Mutex::new(HashSet::new()),
        })
    }
    pub(crate) fn bind(&self, context: &Context) -> LaunchIdentity {
        self.bind_owner(context, format!("synthetic-owner-{}", Uuid::new_v4()))
    }
    fn bind_owner(&self, context: &Context, owner: String) -> LaunchIdentity {
        let identity = LaunchIdentity {
            generation: Uuid::new_v4(),
            owner,
        };
        self.bind_identity(context, &identity);
        self.state(&identity, LaunchOwnerState::Current);
        identity
    }
    fn bind_identity(&self, context: &Context, identity: &LaunchIdentity) {
        self.bindings
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
    }
    pub(crate) fn state(&self, identity: &LaunchIdentity, state: LaunchOwnerState) {
        self.owners
            .lock()
            .unwrap()
            .insert(identity.generation, state);
    }
    fn root_changed(&self, identity: &LaunchIdentity) {
        self.changed.lock().unwrap().insert(identity.generation);
    }
    fn root_fenced(&self, identity: &LaunchIdentity) {
        self.fenced.lock().unwrap().insert(identity.generation);
    }
    pub(crate) fn app(self: &Arc<Self>, f: &Fixture) -> Application {
        Application::new(f.store.clone(), f.clock.clone(), self.clone())
    }
}
#[async_trait]
impl HostGate for LaunchGate {
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
    async fn verify(&self, context: &Context) -> Result<HostState> {
        let probe = self.writer_probe.lock().unwrap().clone();
        if let Some(store) = probe {
            let tx = tokio::time::timeout(std::time::Duration::from_secs(2), store.begin())
                .await
                .expect("Join host verification must not hold the storage writer")?;
            tx.rollback().await?;
        }
        self.host.verify(context).await
    }
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self
            .bindings
            .lock()
            .unwrap()
            .get(&context.context_id)
            .cloned())
    }
    async fn launch_name(&self, _context: &Context) -> Result<Option<String>> {
        Ok(self.default_name.lock().unwrap().clone())
    }
    fn launch_owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
        self.owners
            .lock()
            .unwrap()
            .get(&identity.generation)
            .copied()
            .unwrap_or(LaunchOwnerState::Unknown)
    }
    fn launch_root_changed(
        &self,
        _project: Uuid,
        _host: agentchat_core::model::HostKind,
        _root: &str,
        identity: &LaunchIdentity,
    ) -> bool {
        self.fenced.lock().unwrap().contains(&identity.generation)
            || self.changed.lock().unwrap().contains(&identity.generation)
    }
    fn launch_root_departed(
        &self,
        _project: Uuid,
        _host: agentchat_core::model::HostKind,
        _root: &str,
        identity: &LaunchIdentity,
    ) -> bool {
        self.changed.lock().unwrap().contains(&identity.generation)
    }
}

async fn drain_notices(app: &Application, context: &Context) {
    for _ in 0..200 {
        let Some(dispatch) = app.prepare_native(context).await.unwrap() else {
            return;
        };
        assert!(
            matches!(
                &dispatch,
                agentchat_core::delivery::NativeDispatch::Notice(_)
            ),
            "fixture must drain only pre-body typed notices"
        );
        assert!(
            app.finish_native(
                context,
                &dispatch,
                Ok(agentchat_core::delivery::MessageProgress::Accepted(
                    HostReceipt::default()
                ))
            )
            .await
            .unwrap()
        );
    }
    panic!("bounded fixture notice drain exceeded 200 inputs");
}

pub(crate) async fn execute(
    app: &Application,
    context: &Context,
    command: Mutation,
) -> serde_json::Value {
    app.execute(Uuid::new_v4(), context, command)
        .await
        .unwrap()
        .result
}

#[tokio::test]
async fn verified_managed_activation_fences_predecessor_and_replays_typed_result() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let first_context = f.context();
    let first_identity = gate.bind(&first_context);
    let first_request = ManagedActivationRequest {
        request_id: Uuid::new_v4(),
        target_context: first_context.clone(),
        cause: ManagedActivationCause::Initial,
        name: Some("managed-first".into()),
    };
    let first = app
        .activate_managed(
            app.verify_managed_activation(first_request.clone())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.context_id, first_context.context_id);
    assert_eq!(first.generation, first_identity.generation);

    let successor_context = f.context();
    let successor_identity = gate.bind_owner(&successor_context, first_identity.owner.clone());
    gate.root_changed(&first_identity);
    let request = ManagedActivationRequest {
        request_id: Uuid::new_v4(),
        target_context: successor_context.clone(),
        cause: ManagedActivationCause::DistinctRoot {
            predecessor_context: first.context_id,
            predecessor_generation: first.generation,
            transition_epoch: 1,
        },
        name: Some("managed-successor".into()),
    };
    let result = app
        .activate_managed(
            app.verify_managed_activation(request.clone())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.context_id, successor_context.context_id);
    assert_eq!(result.generation, successor_identity.generation);
    assert_ne!(result.agent_id, first.agent_id);
    let replay = app
        .activate_managed(
            app.verify_managed_activation(request.clone())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay, result);

    let mut changed = request;
    changed.name = Some("different-name".into());
    assert_eq!(
        app.activate_managed(app.verify_managed_activation(changed).await.unwrap(),)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    let mut tx = f.store.begin().await.unwrap();
    let retired = tx.launch(first.generation).await.unwrap().unwrap();
    assert_eq!(retired.retirement_reason.as_deref(), Some("root_changed"));
    assert!(
        tx.launch(successor_identity.generation)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none()
    );
}

#[tokio::test]
async fn managed_activation_uses_the_current_reattached_predecessor_context() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let predecessor_context = f.context();
    let predecessor_identity = gate.bind(&predecessor_context);
    let predecessor = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: predecessor_context.clone(),
            cause: ManagedActivationCause::Initial,
            name: Some("reattached-predecessor".into()),
        })
        .await
        .unwrap();

    let mut reattached = predecessor_context.clone();
    reattached.context_id = Uuid::new_v4();
    reattached.incarnation = Uuid::new_v4();
    gate.bind_identity(&reattached, &predecessor_identity);
    assert_eq!(
        app.attach(&reattached).await.unwrap().unwrap().id,
        predecessor.agent_id
    );

    let target = f.context();
    let target_identity = gate.bind_owner(&target, predecessor_identity.owner.clone());
    gate.root_changed(&predecessor_identity);
    let stale_error = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: target.clone(),
            cause: ManagedActivationCause::DistinctRoot {
                predecessor_context: predecessor.context_id,
                predecessor_generation: predecessor.generation,
                transition_epoch: 1,
            },
            name: Some("reattached-successor".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(stale_error.code, ErrorCode::IdentityConflict);

    let result = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: target.clone(),
            cause: ManagedActivationCause::DistinctRoot {
                predecessor_context: reattached.context_id,
                predecessor_generation: predecessor.generation,
                transition_epoch: 2,
            },
            name: Some("reattached-successor".into()),
        })
        .await
        .unwrap();
    assert_eq!(result.context_id, target.context_id);
    assert_eq!(result.generation, target_identity.generation);
}

#[tokio::test]
async fn managed_activation_refuses_an_older_generation_with_the_current_session_context() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let original_context = f.context();
    let original_identity = gate.bind(&original_context);
    let original = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: original_context.clone(),
            cause: ManagedActivationCause::Initial,
            name: Some("generation-predecessor".into()),
        })
        .await
        .unwrap();
    gate.root_changed(&original_identity);
    assert_eq!(
        app.reconcile_launch(original.generation)
            .await
            .unwrap()
            .result["retired"],
        true
    );
    gate.state(&original_identity, LaunchOwnerState::Gone);

    let mut replacement_context = original_context;
    replacement_context.context_id = Uuid::new_v4();
    replacement_context.incarnation = Uuid::new_v4();
    let replacement_identity = gate.bind(&replacement_context);
    let replacement = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: replacement_context.clone(),
            cause: ManagedActivationCause::Initial,
            name: None,
        })
        .await
        .unwrap();
    assert_eq!(replacement.agent_id, original.agent_id);
    assert_eq!(replacement.generation, replacement_identity.generation);

    let target = f.context();
    let target_identity = gate.bind_owner(&target, original_identity.owner);
    let error = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: target,
            cause: ManagedActivationCause::DistinctRoot {
                predecessor_context: replacement_context.context_id,
                predecessor_generation: original.generation,
                transition_epoch: 3,
            },
            name: Some("stale-generation-successor".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdentityConflict);
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        tx.launch(target_identity.generation)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        tx.latest_launch(original.agent_id)
            .await
            .unwrap()
            .unwrap()
            .identity,
        replacement_identity
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn managed_activation_refuses_a_fabricated_distinct_root_without_departure() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let predecessor_context = f.context();
    let predecessor_identity = gate.bind(&predecessor_context);
    let predecessor = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: predecessor_context,
            cause: ManagedActivationCause::Initial,
            name: Some("current-predecessor".into()),
        })
        .await
        .unwrap();
    let target = f.context();
    gate.bind_owner(&target, predecessor_identity.owner.clone());
    // A Codex transition fence suspends the predecessor but is still
    // rollback-capable, so it is not positive distinct-root authority.
    gate.root_fenced(&predecessor_identity);

    let error = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: target,
            cause: ManagedActivationCause::DistinctRoot {
                predecessor_context: predecessor.context_id,
                predecessor_generation: predecessor.generation,
                transition_epoch: 1,
            },
            name: Some("fabricated-successor".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdentityConflict);
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        tx.launch(predecessor_identity.generation)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn provisional_root_fence_does_not_permanently_retire_a_launch() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let context = f.context();
    let identity = gate.bind(&context);
    let activated = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: context,
            cause: ManagedActivationCause::Initial,
            name: Some("rollback-capable".into()),
        })
        .await
        .unwrap();

    gate.root_fenced(&identity);
    assert_eq!(
        app.reconcile_launch(activated.generation)
            .await
            .unwrap()
            .result["retired"],
        false
    );
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        tx.launch(activated.generation)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn managed_activation_accepts_the_exact_predecessor_already_fenced_by_departure() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let predecessor_context = f.context();
    let predecessor_identity = gate.bind(&predecessor_context);
    let predecessor = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: predecessor_context,
            cause: ManagedActivationCause::Initial,
            name: Some("fenced-predecessor".into()),
        })
        .await
        .unwrap();
    gate.root_changed(&predecessor_identity);
    assert_eq!(
        app.reconcile_launch(predecessor.generation)
            .await
            .unwrap()
            .result["retired"],
        true
    );

    let target = f.context();
    let target_identity = gate.bind_owner(&target, predecessor_identity.owner);
    let activated = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: target.clone(),
            cause: ManagedActivationCause::DistinctRoot {
                predecessor_context: predecessor.context_id,
                predecessor_generation: predecessor.generation,
                transition_epoch: 2,
            },
            name: Some("next-root".into()),
        })
        .await
        .unwrap();
    assert_eq!(activated.context_id, target.context_id);
    assert_eq!(activated.generation, target_identity.generation);
}

#[tokio::test]
async fn managed_activation_refuses_an_exact_predecessor_owned_by_another_process() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let predecessor_context = f.context();
    let predecessor_identity = gate.bind(&predecessor_context);
    let predecessor = app
        .activate_managed(
            app.verify_managed_activation(ManagedActivationRequest {
                request_id: Uuid::new_v4(),
                target_context: predecessor_context,
                cause: ManagedActivationCause::Initial,
                name: Some("owner-a".into()),
            })
            .await
            .unwrap(),
        )
        .await
        .unwrap();
    let target_context = f.context();
    gate.bind(&target_context);
    let error = app
        .activate_managed(
            app.verify_managed_activation(ManagedActivationRequest {
                request_id: Uuid::new_v4(),
                target_context,
                cause: ManagedActivationCause::DistinctRoot {
                    predecessor_context: predecessor.context_id,
                    predecessor_generation: predecessor.generation,
                    transition_epoch: 1,
                },
                name: Some("owner-b".into()),
            })
            .await
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdentityConflict);
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        tx.launch(predecessor_identity.generation)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn managed_activation_retry_replays_committed_receipt_after_owner_loss() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let context = f.context();
    let identity = gate.bind(&context);
    let request = ManagedActivationRequest {
        request_id: Uuid::new_v4(),
        target_context: context,
        cause: ManagedActivationCause::Initial,
        name: Some("durable-replay".into()),
    };
    let first = app.activate_managed_request(request.clone()).await.unwrap();
    gate.state(&identity, LaunchOwnerState::Gone);
    let replay = app.activate_managed_request(request).await.unwrap();
    assert_eq!(replay, first);
    assert_eq!(f.count("launches").await, 1);
}

#[tokio::test]
async fn managed_initial_cannot_rejoin_a_same_root_left_conversation() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let context = f.context();
    let identity = gate.bind(&context);
    app.activate_managed_request(ManagedActivationRequest {
        request_id: Uuid::new_v4(),
        target_context: context.clone(),
        cause: ManagedActivationCause::Initial,
        name: Some("same-root-left".into()),
    })
    .await
    .unwrap();
    execute(&app, &context, Mutation::Leave).await;

    let mut fresh = context;
    fresh.context_id = Uuid::new_v4();
    fresh.incarnation = Uuid::new_v4();
    gate.bind_owner(&fresh, identity.owner);
    let error = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: fresh,
            cause: ManagedActivationCause::Initial,
            name: Some("same-root-left".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdentityConflict);
    let mut tx = f.store.begin().await.unwrap();
    assert!(!tx.handle("same-root-left").await.unwrap().unwrap().joined);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn managed_distinct_root_rejoins_after_an_explicit_leave() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let predecessor_context = f.context();
    let predecessor_identity = gate.bind(&predecessor_context);
    let predecessor = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: predecessor_context.clone(),
            cause: ManagedActivationCause::Initial,
            name: Some("left-predecessor".into()),
        })
        .await
        .unwrap();
    execute(&app, &predecessor_context, Mutation::Leave).await;

    let target = f.context();
    let target_identity = gate.bind_owner(&target, predecessor_identity.owner.clone());
    let uncorrelated = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: target.clone(),
            cause: ManagedActivationCause::DistinctRoot {
                predecessor_context: predecessor.context_id,
                predecessor_generation: predecessor.generation,
                transition_epoch: 1,
            },
            name: Some("left-successor".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(uncorrelated.code, ErrorCode::IdentityConflict);
    gate.root_changed(&predecessor_identity);
    let activated = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: target.clone(),
            cause: ManagedActivationCause::DistinctRoot {
                predecessor_context: predecessor.context_id,
                predecessor_generation: predecessor.generation,
                transition_epoch: 2,
            },
            name: Some("left-successor".into()),
        })
        .await
        .unwrap();

    assert_eq!(activated.context_id, target.context_id);
    assert_eq!(activated.generation, target_identity.generation);
    assert_ne!(activated.agent_id, predecessor.agent_id);
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.launch(predecessor.generation)
            .await
            .unwrap()
            .unwrap()
            .retirement_reason
            .as_deref(),
        Some("explicit_leave")
    );
    assert!(tx.agent(activated.agent_id).await.unwrap().unwrap().joined);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn new_managed_owner_can_resume_a_retained_root_after_positive_owner_loss() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let context = f.context();
    let original_identity = gate.bind(&context);
    let original = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: context.clone(),
            cause: ManagedActivationCause::Initial,
            name: Some("retained-root".into()),
        })
        .await
        .unwrap();
    gate.state(&original_identity, LaunchOwnerState::Gone);

    let mut resumed = context;
    resumed.context_id = Uuid::new_v4();
    resumed.incarnation = Uuid::new_v4();
    let resumed_identity = gate.bind(&resumed);
    let result = app
        .activate_managed_request(ManagedActivationRequest {
            request_id: Uuid::new_v4(),
            target_context: resumed.clone(),
            cause: ManagedActivationCause::Initial,
            name: None,
        })
        .await
        .unwrap();

    assert_eq!(result.agent_id, original.agent_id);
    assert_eq!(result.generation, resumed_identity.generation);
    let who = app.query(Some(&resumed), Query::Whoami).await.unwrap();
    let encoded = serde_json::to_value(who).unwrap();
    assert_eq!(encoded["actor"]["handle"], "retained-root");
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.launch(original_identity.generation)
            .await
            .unwrap()
            .unwrap()
            .retirement_reason
            .as_deref(),
        Some("owner_lost")
    );
    tx.rollback().await.unwrap();
}
async fn pending(app: &Application, context: &Context) -> Vec<serde_json::Value> {
    app.query(
        Some(context),
        Query::Inbox {
            page: PageRequest::default(),
        },
    )
    .await
    .map(|value| serde_json::to_value(value).unwrap())
    .unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

async fn delivery_states(
    f: &Fixture,
) -> Vec<(String, String, Option<String>, i32, Option<String>)> {
    let mut rows = f
        .rows("deliveries")
        .await
        .into_iter()
        .map(|r| {
            (
                r.try_get("", "id").unwrap(),
                r.try_get("", "state").unwrap(),
                r.try_get("", "wake_batch_id").unwrap(),
                r.try_get("", "attempts").unwrap(),
                r.try_get("", "delivered_at").unwrap(),
            )
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

#[tokio::test]
async fn explicit_join_creates_authority_and_retries_cannot_move_or_reopen_it() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    let a = f.context();
    let join = Mutation::Join {
        name: Some("Alpha".into()),
    };
    assert_eq!(
        app.execute(Uuid::new_v4(), &a, join.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        app.start_launch(&a, Some("alpha")).await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    let identity = gate.bind(&a);
    // A verified readiness binding alone creates no business rows.
    assert_eq!(f.count("actors").await, 0);
    assert_eq!(f.count("launches").await, 0);
    let started = app.execute(Uuid::new_v4(), &a, join.clone()).await.unwrap();
    let counts = f.counts().await;
    assert_eq!(
        app.start_launch(&a, Some("alpha")).await.unwrap().result,
        started.result
    );
    assert_eq!(
        app.start_launch(&a, Some("changed-name"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdentityConflict
    );
    assert_eq!(counts, f.counts().await);
    assert_eq!(f.count("launches").await, 1);
    let mut fork = a.clone();
    fork.context_id = Uuid::new_v4();
    fork.host_session_id = Uuid::new_v4().to_string();
    fork.incarnation = Uuid::new_v4();
    gate.bindings
        .lock()
        .unwrap()
        .insert(fork.context_id, identity.clone());
    assert!(app.start_launch(&fork, None).await.is_err());
    execute(&app, &a, Mutation::Leave).await;
    assert!(app.start_launch(&a, None).await.is_err());
    assert!(app.execute(Uuid::new_v4(), &a, join).await.is_err());
    assert_eq!(
        f.rows("launches").await[0]
            .try_get::<String>("", "retirement_reason")
            .unwrap(),
        "explicit_leave"
    );
}

#[tokio::test]
async fn uncertainty_suspends_without_discard_and_positive_loss_cancels_before_resume() {
    let f = Fixture::new().await;
    let sender = f.context();
    f.join(&sender, "sender").await;
    let receiver = f.context();
    let gate = LaunchGate::new(&f);
    let identity = gate.bind(&receiver);
    let app = gate.app(&f);
    let receiver_id = id(
        &app.start_launch(&receiver, Some("receiver"))
            .await
            .unwrap()
            .result,
        "agent_id",
    );
    let first = id(
        &f.post(&sender, "@receiver before loss").await.result,
        "message_id",
    );
    let dispatch = app.prepare_wake(&receiver).await.unwrap().unwrap();
    let delivery = f.delivery(first, receiver_id).await;
    let before = delivery_states(&f).await;
    gate.state(&identity, LaunchOwnerState::Unknown);
    assert!(app.prepare_wake(&receiver).await.is_err());
    assert!(
        !app.wake_is_current(
            &receiver,
            dispatch.hint.batch_id,
            dispatch.token,
            &dispatch
                .hint
                .events
                .iter()
                .map(|event| event.delivery_id)
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap()
    );
    assert_eq!(app.maintain().await.unwrap().retired_launches, 0);
    assert_eq!(before, delivery_states(&f).await);
    gate.state(&identity, LaunchOwnerState::Current);
    assert!(
        app.wake_is_current(
            &receiver,
            dispatch.hint.batch_id,
            dispatch.token,
            &dispatch
                .hint
                .events
                .iter()
                .map(|event| event.delivery_id)
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap()
    );
    let unrelated = gate.bind(&receiver);
    assert_ne!(unrelated.generation, identity.generation);
    assert!(
        !app.wake_is_current(
            &receiver,
            dispatch.hint.batch_id,
            dispatch.token,
            &dispatch
                .hint
                .events
                .iter()
                .map(|event| event.delivery_id)
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap()
    );
    gate.bindings
        .lock()
        .unwrap()
        .insert(receiver.context_id, identity.clone());
    gate.state(&identity, LaunchOwnerState::Gone);
    assert!(
        app.reconcile_launch(identity.generation)
            .await
            .unwrap()
            .result["retired"]
            .as_bool()
            .unwrap()
    );
    assert!(
        !app.finish_wake(&receiver, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    let cancelled = f
        .rows("deliveries")
        .await
        .into_iter()
        .find(|r| r.try_get::<String>("", "id").unwrap() == delivery.to_string())
        .unwrap();
    assert_eq!(
        cancelled.try_get::<String>("", "state").unwrap(),
        "failed_left"
    );
    let inactive = id(
        &f.post(&sender, "@receiver inactive interval").await.result,
        "message_id",
    );
    let mut resumed = receiver.clone();
    resumed.context_id = Uuid::new_v4();
    resumed.incarnation = Uuid::new_v4();
    assert!(app.attach(&resumed).await.is_err());
    let successor = gate.bind(&resumed);
    let restarted = app.start_launch(&resumed, None).await.unwrap();
    assert_eq!(id(&restarted.result, "agent_id"), receiver_id);
    assert!(
        !app.reconcile_launch(identity.generation)
            .await
            .unwrap()
            .result["retired"]
            .as_bool()
            .unwrap()
    );
    assert!(app.heartbeat(&receiver).await.is_err());
    assert!(
        !app.finish_wake(&receiver, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    let fresh = id(
        &f.post(&sender, "@receiver fresh only").await.result,
        "message_id",
    );
    let inbox = pending(&app, &resumed).await;
    let encoded = serde_json::to_string(&inbox).unwrap();
    assert!(encoded.contains(&fresh.to_string()));
    assert!(!encoded.contains(&inactive.to_string()));
    // Cancelled rows remain explicit history in an all-state diagnostic inbox;
    // no old body is selected by the new native wake claim.
    let fresh_dispatch = app.prepare_wake(&resumed).await.unwrap().unwrap();
    assert_eq!(
        fresh_dispatch
            .hint
            .events
            .iter()
            .map(|e| e.message_id)
            .collect::<Vec<_>>(),
        vec![fresh]
    );
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        tx.launch(identity.generation)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_some()
    );
    assert!(
        tx.launch(successor.generation)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_none()
    );
}

#[tokio::test]
async fn retirement_preserves_deliveries_reads_and_independent_recipient_timeout_history_through_backup()
 {
    let f = Fixture::new().await;
    let recipient = f.context();
    let author = f.context();
    let gate = LaunchGate::new(&f);
    let identity = gate.bind(&author);
    gate.bind(&recipient);
    let app = gate.app(&f);
    let recipient_id = id(
        &app.start_launch(&recipient, Some("recipient"))
            .await
            .unwrap()
            .result,
        "agent_id",
    );
    let author_id = id(
        &app.start_launch(&author, Some("author"))
            .await
            .unwrap()
            .result,
        "agent_id",
    );
    drain_notices(&app, &recipient).await;
    let old = id(
        &execute(
            &app,
            &recipient,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "@author delivered and read".into(),
            },
        )
        .await,
        "message_id",
    );
    let delivery = f.delivery(old, author_id).await;
    let original = app.prepare_message(&author).await.unwrap().unwrap();
    app.finish_message(&author, &original, Ok(HostReceipt::default()))
        .await
        .unwrap();
    execute(
        &app,
        &author,
        Mutation::Read {
            message_ids: vec![old],
        },
    )
    .await;
    f.timeout(1).await;
    let watched = execute(
        &app,
        &author,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@recipient read deadline".into(),
        },
    )
    .await;
    let watched_id = id(&watched, "message_id");
    app.prepare_message(&recipient).await.unwrap().unwrap();
    gate.state(&identity, LaunchOwnerState::Gone);
    assert_eq!(app.maintain().await.unwrap().retired_launches, 1);
    assert_eq!(f.count("dispatch_messages").await, 2);
    assert_eq!(f.count("read_receipts").await, 1);
    let delivered = f
        .rows("deliveries")
        .await
        .into_iter()
        .find(|r| r.try_get::<String>("", "id").unwrap() == delivery.to_string())
        .unwrap();
    assert_eq!(
        delivered.try_get::<String>("", "state").unwrap(),
        "delivered"
    );
    f.clock.advance(2);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        1
    );
    let notices = f.rows("delivery_timeout_notices").await;
    let notice = notices
        .iter()
        .find(|row| row.try_get::<String>("", "message_id").unwrap() == watched_id.to_string())
        .unwrap()
        .try_get::<String>("", "notice_dispatch_id")
        .unwrap();
    assert!(!f.rows("deliveries").await.iter().any(|r| {
        r.try_get::<String>("", "dispatch_id").unwrap() == notice
            && r.try_get::<String>("", "recipient_id").unwrap() == author_id.to_string()
    }));
    // Another recipient's original work is independent of author retirement.
    assert!(f.delivery(watched_id, recipient_id).await != Uuid::nil());
    let destination = f.root.path().join("launch-backup.sqlite3");
    f.store.backup(&destination).await.unwrap();
    let restored = Arc::new(
        agentchat_store::Store::open(&destination, f.store.identity().clone())
            .await
            .unwrap(),
    );
    let restored_app = Application::new(restored.clone(), f.clock.clone(), gate.clone());
    restored_app.invalidate_connections().await.unwrap();
    assert!(
        !restored_app
            .reconcile_launch(identity.generation)
            .await
            .unwrap()
            .result["retired"]
            .as_bool()
            .unwrap()
    );
    assert!(restored_app.start_launch(&author, None).await.is_err());
    let mut tx = restored.begin().await.unwrap();
    let launch = tx.launch(identity.generation).await.unwrap().unwrap();
    assert!(launch.retired_at.is_some());
    assert_eq!(
        tx.receipt(old, author_id)
            .await
            .unwrap()
            .unwrap()
            .message_id,
        old
    );
    assert_eq!(
        tx.delivery_timeout_notice(watched_id)
            .await
            .unwrap()
            .unwrap()
            .to_string(),
        notice
    );
}

#[tokio::test]
async fn same_live_generation_reconnects_without_transferring_owner_or_reviving_after_leave() {
    let f = Fixture::new().await;
    let context = f.context();
    let gate = LaunchGate::new(&f);
    let identity = gate.bind(&context);
    let app = gate.app(&f);
    let started = app.start_launch(&context, Some("owner")).await.unwrap();
    let mut reconnect = context.clone();
    reconnect.context_id = Uuid::new_v4();
    reconnect.incarnation = Uuid::new_v4();
    gate.bindings
        .lock()
        .unwrap()
        .insert(reconnect.context_id, identity.clone());
    assert!(app.attach(&reconnect).await.unwrap().unwrap().joined);
    assert!(app.heartbeat(&context).await.is_err());
    let competing = gate.bind(&context);
    assert_eq!(
        app.start_launch(&context, None).await.unwrap_err().code,
        ErrorCode::IdentityConflict
    );
    gate.bindings
        .lock()
        .unwrap()
        .insert(context.context_id, identity.clone());
    execute(&app, &reconnect, Mutation::Leave).await;
    assert!(app.attach(&context).await.is_err());
    assert!(app.start_launch(&context, None).await.is_err());
    // A separately verified AGENTCHAT-managed launch may reuse the retained handle.
    let mut fresh = context.clone();
    fresh.context_id = Uuid::new_v4();
    fresh.incarnation = Uuid::new_v4();
    gate.bindings
        .lock()
        .unwrap()
        .insert(fresh.context_id, competing);
    assert_eq!(
        id(
            &app.start_launch(&fresh, None).await.unwrap().result,
            "agent_id"
        ),
        id(&started.result, "agent_id")
    );
}

#[tokio::test]
async fn owner_loss_stops_lease_renewal_without_moving_the_original_expiry() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let context = f.context();
    let identity = gate.bind(&context);
    let app = gate.app(&f);
    app.start_launch(&context, Some("owner")).await.unwrap();
    let lease = execute(
        &app,
        &context,
        Mutation::Reserve {
            patterns: vec!["src/**".into()],
            ttl_seconds: Some(30),
            reason: None,
        },
    )
    .await;
    let lease_id = id(&lease, "reservation_id");
    gate.state(&identity, LaunchOwnerState::Unknown);
    f.clock.advance(11);
    assert_eq!(app.maintain().await.unwrap().renewed, 0);
    let expiry = f.rows("reservations").await[0]
        .try_get::<String>("", "expires_at")
        .unwrap();
    gate.state(&identity, LaunchOwnerState::Gone);
    app.reconcile_launch(identity.generation).await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.reservation(lease_id).await.unwrap().unwrap().state,
        agentchat_core::model::LeaseState::Active
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        f.rows("reservations").await[0]
            .try_get::<String>("", "expires_at")
            .unwrap(),
        expiry
    );
    f.clock.advance(20);
    assert_eq!(app.maintain().await.unwrap().expired, 1);
}

#[tokio::test]
async fn bounded_owner_scan_advances_past_thirty_two_live_front_entries() {
    let f = Fixture::new().await;
    let gate = LaunchGate::new(&f);
    let app = gate.app(&f);
    for i in 0..35 {
        let context = f.context();
        gate.bind(&context);
        app.start_launch(&context, Some(&format!("owner-{i}")))
            .await
            .unwrap();
    }
    let mut tx = f.store.begin().await.unwrap();
    let first = tx.active_launches(None).await.unwrap();
    assert_eq!(first.len(), 32);
    let last = tx
        .active_launches(Some(first.last().unwrap().identity.generation))
        .await
        .unwrap();
    assert_eq!(last.len(), 3);
    tx.rollback().await.unwrap();
    for launch in last {
        gate.state(&launch.identity, LaunchOwnerState::Gone);
    }
    assert_eq!(app.maintain().await.unwrap().retired_launches, 0);
    assert_eq!(app.maintain().await.unwrap().retired_launches, 3);
}

#[tokio::test]
async fn explicit_leave_after_physical_retirement_releases_leases_without_cancelling_other_recipients()
 {
    let f = Fixture::new().await;
    let other = f.context();
    let context = f.context();
    let gate = LaunchGate::new(&f);
    gate.bind(&other);
    let identity = gate.bind(&context);
    let app = gate.app(&f);
    app.start_launch(&other, Some("other")).await.unwrap();
    let owner = id(
        &app.start_launch(&context, Some("owner"))
            .await
            .unwrap()
            .result,
        "agent_id",
    );
    let lease = execute(
        &app,
        &context,
        Mutation::Reserve {
            patterns: vec!["src/**".into()],
            ttl_seconds: Some(30),
            reason: None,
        },
    )
    .await;
    drain_notices(&app, &other).await;
    f.timeout(1).await;
    let posted = execute(
        &app,
        &context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@other independent recipient processing".into(),
        },
    )
    .await;
    let message = id(&posted, "message_id");
    let input = app.prepare_message(&other).await.unwrap().unwrap();
    let mut tx = f.store.begin().await.unwrap();
    let original = tx.delivery(input.input.delivery_id).await.unwrap().unwrap();
    assert_eq!(
        original.state,
        agentchat_core::model::DeliveryState::Processing
    );
    assert!(
        tx.author_timeout_notice_allowed(owner, input.input.seq)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
    gate.state(&identity, LaunchOwnerState::Gone);
    app.reconcile_launch(identity.generation).await.unwrap();
    let events = f.count("events").await;
    let before_cleanup = app.event_head().await.unwrap();
    for _ in 0..2 {
        execute(&app, &context, Mutation::Leave).await;
        let retained_events = f.rows("events").await;
        assert_eq!(
            retained_events.len(),
            events + 1,
            "first cleanup releases its lease once; repetition creates no new exit or release"
        );
        let added = retained_events
            .iter()
            .filter(|row| row.try_get::<i64>("", "seq").unwrap() > before_cleanup.get())
            .collect::<Vec<_>>();
        assert_eq!(added.len(), 1);
        assert_eq!(
            added[0].try_get::<String>("", "kind").unwrap(),
            "reservation.released"
        );
        assert_eq!(
            added[0].try_get::<Option<String>>("", "actor_id").unwrap(),
            Some(owner.to_string())
        );
        let released = added[0].try_get::<serde_json::Value>("", "data").unwrap();
        assert_eq!(
            id(&released, "reservation_id"),
            id(&lease, "reservation_id")
        );
        let mut tx = f.store.begin().await.unwrap();
        assert!(
            !tx.author_timeout_notice_allowed(owner, input.input.seq)
                .await
                .unwrap()
        );
        assert_eq!(
            tx.delivery(input.input.delivery_id).await.unwrap().unwrap(),
            original
        );
        assert_eq!(
            tx.reservation(id(&lease, "reservation_id"))
                .await
                .unwrap()
                .unwrap()
                .state,
            agentchat_core::model::LeaseState::Released
        );
        tx.rollback().await.unwrap();
    }
    // A separately verified new owner may rejoin the retained conversation,
    // but ordinary actor saving must preserve the explicit author cancellation.
    let resumed = Context {
        context_id: Uuid::new_v4(),
        incarnation: Uuid::new_v4(),
        ..context.clone()
    };
    gate.bind(&resumed);
    assert_eq!(
        id(
            &app.start_launch(&resumed, None).await.unwrap().result,
            "agent_id"
        ),
        owner
    );
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        !tx.author_timeout_notice_allowed(owner, input.input.seq)
            .await
            .unwrap()
    );
    assert_eq!(
        tx.delivery(input.input.delivery_id).await.unwrap().unwrap(),
        original
    );
    assert_eq!(
        tx.message(message).await.unwrap().unwrap().body,
        "@other independent recipient processing"
    );
    tx.rollback().await.unwrap();
    assert_eq!(f.count("read_receipts").await, 0);
    assert_eq!(f.count("delivery_timeout_notices").await, 0);
    let retired = f
        .rows("launches")
        .await
        .into_iter()
        .find(|row| {
            row.try_get::<String>("", "generation").unwrap() == identity.generation.to_string()
        })
        .unwrap();
    assert_eq!(
        retired.try_get::<String>("", "retirement_reason").unwrap(),
        "owner_lost"
    );
}

#[path = "launches/explicit_join.rs"]
mod explicit_join;
