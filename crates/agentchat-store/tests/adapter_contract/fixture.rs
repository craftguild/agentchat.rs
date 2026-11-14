//! Test-only authority and three mechanism combinations; no native admission.
use super::support::{Fixture, id};
use agentchat_core::{
    Result,
    application::Application,
    delivery::{
        ActiveInput, HostReceipt, InputCapabilities, InputRecovery, MessageDispatch, MessagePort,
        MessageProgress, NativeDispatch, NoticeDispatch,
    },
    model::{Context, HostKind, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState, Storage},
};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

pub trait Mechanism: Send + Sync + Default {
    fn capabilities(&self) -> InputCapabilities;
}
#[derive(Default)]
pub struct Direct;
impl Mechanism for Direct {
    fn capabilities(&self) -> InputCapabilities {
        InputCapabilities::complete(InputRecovery::RetainUncertainty, ActiveInput::ExactTurnId)
    }
}
#[derive(Default)]
pub struct Journal;
impl Mechanism for Journal {
    fn capabilities(&self) -> InputCapabilities {
        InputCapabilities::complete(InputRecovery::OriginalReceipt, ActiveInput::WithoutTurnId)
    }
}
#[derive(Default)]
pub struct TurnBoundJournal;
impl Mechanism for TurnBoundJournal {
    fn capabilities(&self) -> InputCapabilities {
        let mut capabilities = Journal.capabilities();
        capabilities.active_input = ActiveInput::ExactTurnId;
        capabilities
    }
}

pub struct Adapter<M> {
    pub mechanism: M,
    pub capabilities: Mutex<InputCapabilities>,
    pub bindings: Mutex<HashMap<Uuid, LaunchIdentity>>,
    pub owner: Mutex<LaunchOwnerState>,
    pub root_changed: Mutex<bool>,
    pub state: Mutex<HostState>,
    pub calls: Mutex<Vec<(Uuid, bool, String)>>,
    pub receipts: Mutex<HashMap<Uuid, MessageProgress>>,
    pub outcome: Mutex<Option<Result<MessageProgress>>>,
    pub hold_result: AtomicBool,
    pub hold_verification: AtomicBool,
    pub verification_entered: tokio::sync::Notify,
    pub verification_release: tokio::sync::Notify,
}
impl<M: Mechanism> Adapter<M> {
    fn new() -> Self {
        let mechanism = M::default();
        Self {
            capabilities: Mutex::new(mechanism.capabilities()),
            mechanism,
            bindings: Mutex::default(),
            owner: Mutex::new(LaunchOwnerState::Current),
            root_changed: Mutex::new(false),
            state: Mutex::new(HostState {
                phase: Phase::Idle,
                active_turn_id: None,
                wake_paused: false,
            }),
            calls: Mutex::default(),
            receipts: Mutex::default(),
            outcome: Mutex::new(None),
            hold_result: AtomicBool::new(false),
            hold_verification: AtomicBool::new(false),
            verification_entered: tokio::sync::Notify::new(),
            verification_release: tokio::sync::Notify::new(),
        }
    }
    pub fn bind(&self, context: &Context) -> LaunchIdentity {
        let identity = LaunchIdentity {
            generation: Uuid::new_v4(),
            owner: "test-only-adapter-contract".into(),
        };
        self.bindings
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
        identity
    }
    fn present(&self, token: Uuid, polling: bool, text: String) -> Result<MessageProgress> {
        self.calls.lock().unwrap().push((token, polling, text));
        if polling {
            if self.mechanism.capabilities().recovery != InputRecovery::OriginalReceipt {
                return Err(agentchat_core::Error::new(
                    agentchat_core::ErrorCode::UnsupportedProtocol,
                    "No original result journal.",
                ));
            }
            return Ok(self
                .receipts
                .lock()
                .unwrap()
                .get(&token)
                .cloned()
                .unwrap_or(MessageProgress::AwaitingAcceptance));
        }
        self.outcome
            .lock()
            .unwrap()
            .take()
            .unwrap_or(Ok(MessageProgress::Accepted(HostReceipt::default())))
    }
}
#[async_trait]
impl<M: Mechanism> HostGate for Adapter<M> {
    fn input_capabilities(&self, _: &Context) -> Result<InputCapabilities> {
        Ok(*self.capabilities.lock().unwrap())
    }
    async fn verify(&self, _: &Context) -> Result<HostState> {
        if self.hold_verification.swap(false, Ordering::SeqCst) {
            self.verification_entered.notify_one();
            self.verification_release.notified().await;
        }
        Ok(self.state.lock().unwrap().clone())
    }
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self
            .bindings
            .lock()
            .unwrap()
            .get(&context.context_id)
            .cloned())
    }
    fn launch_owner(&self, _: &LaunchIdentity) -> LaunchOwnerState {
        *self.owner.lock().unwrap()
    }
    fn launch_root_changed(&self, _: Uuid, _: HostKind, _: &str, _: &LaunchIdentity) -> bool {
        *self.root_changed.lock().unwrap()
    }
}
#[async_trait]
impl<M: Mechanism> MessagePort for Adapter<M> {
    async fn present_message(
        &self,
        _: &Context,
        dispatch: &MessageDispatch,
    ) -> Result<MessageProgress> {
        let result = self.present(
            dispatch.token,
            dispatch.awaiting_acceptance,
            dispatch.input.text()?,
        );
        if self.hold_result.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        result
    }
    async fn present_notice(
        &self,
        _: &Context,
        dispatch: &NoticeDispatch,
    ) -> Result<MessageProgress> {
        self.present(
            dispatch.token,
            dispatch.awaiting_acceptance,
            dispatch.input.text()?,
        )
    }
}

pub struct Harness<M> {
    pub f: Fixture,
    pub adapter: Arc<Adapter<M>>,
    pub app: Application,
    pub sender: Context,
    pub receiver: Context,
    pub identity: LaunchIdentity,
}
impl<M: Mechanism + 'static> Harness<M> {
    pub async fn new(label: HostKind) -> Self {
        let f = Fixture::new().await;
        let sender = f.context();
        f.join(&sender, "sender").await;
        let adapter = Arc::new(Adapter::<M>::new());
        let mut receiver = f.context();
        receiver.host = label;
        let identity = adapter.bind(&receiver);
        let app = Application::new(f.store.clone(), f.clock.clone(), adapter.clone());
        app.start_launch(&receiver, Some("receiver")).await.unwrap();
        Self {
            f,
            adapter,
            app,
            sender,
            receiver,
            identity,
        }
    }
    pub async fn send(&self, body: &str) -> Uuid {
        id(&self.f.post(&self.sender, body).await.result, "message_id")
    }
    pub async fn dispatch(&self, dispatch: &NativeDispatch) -> Result<MessageProgress> {
        self.dispatch_in(&self.receiver, dispatch).await
    }
    pub async fn dispatch_in(
        &self,
        context: &Context,
        dispatch: &NativeDispatch,
    ) -> Result<MessageProgress> {
        match dispatch {
            NativeDispatch::Message(dispatch) => {
                self.adapter.present_message(context, dispatch).await
            }
            NativeDispatch::Notice(dispatch) => {
                self.adapter.present_notice(context, dispatch).await
            }
        }
    }
    pub async fn stream(&self) -> agentchat_core::delivery::MessageStream {
        self.f
            .store
            .snapshot()
            .await
            .unwrap()
            .message_stream(self.identity.generation)
            .await
            .unwrap()
            .unwrap()
    }
}
