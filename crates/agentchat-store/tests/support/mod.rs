//! Synthetic application fixtures. They do not admit any real host profile.

#![allow(dead_code)]

mod actor_legacy;
pub mod native;
mod receipt_legacy;
pub mod upgrade;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use agentchat_core::{
    Error, ErrorCode, Result,
    application::{Application, Committed, Mutation},
    model::{Context, HostKind, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState},
    project::{ProjectIdentity, Scope},
    time::{Clock, Timestamp},
};
use agentchat_store::Store;
use async_trait::async_trait;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, QueryResult,
    sea_query::{Alias, Asterisk, Expr, ExprTrait, Query},
};
use serde_json::Value;
use tempfile::TempDir;
use uuid::Uuid;

pub struct TestClock(Mutex<Timestamp>);
impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap()
    }
}
impl TestClock {
    pub fn fixed() -> Self {
        Self(Mutex::new(
            Timestamp::parse("2026-10-15T00:00:00.000Z").unwrap(),
        ))
    }
    pub fn advance(&self, seconds: i64) {
        let mut now = self.0.lock().unwrap();
        *now = now.after_seconds(seconds).unwrap();
    }
}

#[derive(Default)]
pub struct SyntheticHost {
    states: Mutex<HashMap<Uuid, HostState>>,
    native: Mutex<HashMap<Uuid, LaunchIdentity>>,
}
impl SyntheticHost {
    pub fn turn(&self, context: &Context, turn: Option<&str>) {
        self.states.lock().unwrap().insert(
            context.context_id,
            HostState {
                phase: if turn.is_some() {
                    Phase::Active
                } else {
                    Phase::Idle
                },
                active_turn_id: turn.map(Into::into),
                wake_paused: false,
            },
        );
    }
    pub fn phase(&self, context: &Context, phase: Phase) {
        self.states.lock().unwrap().insert(
            context.context_id,
            HostState {
                phase,
                active_turn_id: None,
                wake_paused: phase == Phase::Paused,
            },
        );
    }
}
#[async_trait]
impl HostGate for SyntheticHost {
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
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self
            .native
            .lock()
            .unwrap()
            .get(&context.context_id)
            .cloned())
    }
    fn launch_owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
        if self
            .native
            .lock()
            .unwrap()
            .values()
            .any(|current| current == identity)
        {
            LaunchOwnerState::Current
        } else {
            LaunchOwnerState::Unknown
        }
    }
    fn allows_unmanaged_participation(&self) -> bool {
        true
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        if context.host_version != "test-only" || context.os_profile != "synthetic-test" {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "No real host is admitted by this test fixture.",
            ));
        }
        Ok(self
            .states
            .lock()
            .unwrap()
            .get(&context.context_id)
            .cloned()
            .unwrap_or(HostState {
                phase: Phase::Idle,
                active_turn_id: None,
                wake_paused: false,
            }))
    }
}

pub struct Fixture {
    pub root: TempDir,
    pub store: Arc<Store>,
    pub app: Application,
    pub clock: Arc<TestClock>,
    pub host: Arc<SyntheticHost>,
    pub inspect: DatabaseConnection,
}
impl Fixture {
    pub async fn new() -> Self {
        let root = TempDir::new().unwrap();
        let path = root.path().join("agentchat.sqlite3");
        let store = Arc::new(
            Store::open(
                &path,
                ProjectIdentity {
                    id: Uuid::new_v4(),
                    scope: Scope::Directory,
                },
            )
            .await
            .unwrap(),
        );
        let clock = Arc::new(TestClock::fixed());
        let host = Arc::new(SyntheticHost::default());
        let app = Application::new(store.clone(), clock.clone(), host.clone());
        // Explicit-read fixtures select false intentionally; fresh_format tests
        // separately assert the real new-project default is true.
        app.initialize_with_settings(
            None,
            agentchat_core::project::ReceiptSettingsUpdate {
                auto_read: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut options = ConnectOptions::new("sqlite::memory:");
        options
            .max_connections(1)
            .sqlx_logging(false)
            .map_sqlx_sqlite_opts(move |o| {
                o.filename(&path)
                    .in_memory(false)
                    .shared_cache(false)
                    .create_if_missing(false)
                    .foreign_keys(true)
            });
        let inspect = Database::connect(options).await.unwrap();
        Self {
            root,
            store,
            app,
            clock,
            host,
            inspect,
        }
    }

    pub fn context(&self) -> Context {
        Context {
            context_id: Uuid::new_v4(),
            project_id: self.store.identity().id,
            host: HostKind::Codex,
            host_session_id: Uuid::new_v4().to_string(),
            host_version: "test-only".into(),
            os_profile: "synthetic-test".into(),
            working_directory: PathBuf::from(self.root.path()),
            incarnation: Uuid::new_v4(),
        }
    }
    pub async fn run(&self, context: &Context, command: Mutation) -> Committed {
        self.app
            .execute(Uuid::new_v4(), context, command)
            .await
            .unwrap()
    }
    pub async fn timeout(&self, seconds: u32) {
        self.app
            .initialize_with_settings(
                None,
                agentchat_core::project::ReceiptSettingsUpdate {
                    delivery_confirmation_timeout_seconds: Some(seconds),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    pub async fn health(&self) -> Value {
        serde_json::to_value(self.app.health().await.unwrap()).unwrap()
    }
    pub async fn query(
        &self,
        context: Option<&Context>,
        query: agentchat_core::read::Query,
    ) -> Value {
        self.app
            .query(context, query)
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap()
    }
    pub async fn join(&self, context: &Context, name: &str) -> Uuid {
        id(
            &self
                .run(
                    context,
                    Mutation::Join {
                        name: Some(name.into()),
                    },
                )
                .await
                .result,
            "agent_id",
        )
    }
    /// Establish a synthetic managed owner before any receiving obligation exists.
    pub async fn native_join(&self, context: &Context, name: &str) -> Uuid {
        self.host.native.lock().unwrap().insert(
            context.context_id,
            LaunchIdentity {
                generation: Uuid::new_v4(),
                owner: "test-owned-native-receiver".into(),
            },
        );
        let joined = self.app.start_launch(context, Some(name)).await.unwrap();
        id(&joined.result, "agent_id")
    }
    /// Reattach the same immutable test owner to a new connection incarnation.
    pub fn native_reconnect(&self, original: &Context, next: &Context) {
        let mut bindings = self.host.native.lock().unwrap();
        let identity = bindings
            .get(&original.context_id)
            .cloned()
            .expect("original native owner");
        bindings.insert(next.context_id, identity);
    }
    /// Confirm complete synthetic inputs through the exact production FIFO API.
    /// No direct row mutation, ACK alias, authority renewal or read write occurs.
    pub async fn accept_native_inputs(&self, context: &Context, ids: &[Uuid]) {
        let mut remaining = ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        for _ in 0..200 {
            if remaining.is_empty() {
                return;
            }
            let dispatch = self
                .app
                .prepare_native(context)
                .await
                .unwrap()
                .expect("requested delivery must remain in the original native stream");
            remaining.remove(&dispatch.delivery_id());
            assert!(
                self.app
                    .finish_native(
                        context,
                        &dispatch,
                        Ok(agentchat_core::delivery::MessageProgress::Accepted(
                            Default::default()
                        ))
                    )
                    .await
                    .unwrap()
            );
        }
        panic!("bounded synthetic FIFO drain did not reach all original deliveries");
    }
    pub async fn post(&self, context: &Context, body: &str) -> Committed {
        self.run(
            context,
            Mutation::Post {
                channel: Some("dev".into()),
                body: body.into(),
            },
        )
        .await
    }
    pub async fn rows(&self, table: &str) -> Vec<QueryResult> {
        self.inspect
            .query_all(
                &Query::select()
                    .column(Asterisk)
                    .from(Alias::new(table))
                    .to_owned(),
            )
            .await
            .unwrap()
    }
    pub async fn count(&self, table: &str) -> usize {
        self.rows(table).await.len()
    }
    pub async fn assert_read_kinds(&self, automatic: usize, explicit: usize) {
        let rows = self.rows("read_receipts").await;
        let kinds = rows
            .iter()
            .map(|row| row.try_get::<String>("", "kind").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(kinds.len(), automatic + explicit);
        assert_eq!(
            kinds.iter().filter(|kind| *kind == "automatic").count(),
            automatic
        );
        assert_eq!(
            kinds.iter().filter(|kind| *kind == "explicit").count(),
            explicit
        );
    }
    pub async fn counts(&self) -> std::collections::BTreeMap<String, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for table in [
            "actors",
            "sessions",
            "channels",
            "channel_memberships",
            "dispatches",
            "dispatch_messages",
            "dispatch_notices",
            "mentions",
            "read_receipts",
            "events",
            "deliveries",
            "wake_batches",
            "reservations",
            "reservation_patterns",
            "idempotency",
            "delivery_state_transitions",
            "delivery_timeout_notices",
        ] {
            counts.insert(table.to_owned(), self.count(table).await);
        }
        counts
    }
    pub async fn delivery(&self, message: Uuid, recipient: Uuid) -> Uuid {
        let row = self
            .inspect
            .query_one(
                &Query::select()
                    .column((Alias::new("deliveries"), Alias::new("id")))
                    .from(Alias::new("deliveries"))
                    .and_where(
                        Expr::col((Alias::new("deliveries"), Alias::new("dispatch_id")))
                            .eq(message.to_string()),
                    )
                    .and_where(
                        Expr::col((Alias::new("deliveries"), Alias::new("recipient_id")))
                            .eq(recipient.to_string()),
                    )
                    .to_owned(),
            )
            .await
            .unwrap()
            .unwrap();
        Uuid::parse_str(&row.try_get::<String>("", "id").unwrap()).unwrap()
    }
}

pub fn id(value: &Value, key: &str) -> Uuid {
    Uuid::parse_str(value[key].as_str().unwrap()).unwrap()
}
