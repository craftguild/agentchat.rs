//! Retained business state and complete public-query comparisons for crash tests.
use crate::support::{Fixture, id};
use agentchat_core::{
    application::{Application, Mutation},
    delivery::HostReceipt,
    model::Context,
    ports::Storage,
    read::{PageRequest, Query},
};
use agentchat_store::Store;
use serde_json::Value;
use std::{path::Path, sync::Arc};
use uuid::Uuid;

pub struct Dataset {
    pub f: Fixture,
    alice: Context,
    bob: Context,
    request: Uuid,
    command: Mutation,
    result: Value,
    queries: Vec<Query>,
    pub baseline: Vec<Value>,
    pub high_water: i64,
}
impl Dataset {
    pub async fn new() -> Self {
        let f = Fixture::new().await;
        let alice = f.context();
        let bob = f.context();
        f.join(&alice, "alice").await;
        let gate = crate::support::native::Gate::new(&f);
        gate.bind(&bob);
        let native = gate.app(&f);
        native.start_launch(&bob, Some("bob")).await.unwrap();
        f.run(
            &alice,
            Mutation::ChannelCreate {
                name: "second".into(),
            },
        )
        .await;
        f.timeout(600).await;
        let request = Uuid::new_v4();
        let command = Mutation::Post {
            channel: Some("dev".into()),
            body: format!("@bob {}", "x".repeat(65_536 - 5)),
        };
        let posted = f
            .app
            .execute(request, &alice, command.clone())
            .await
            .unwrap();
        let root = id(&posted.result, "message_id");
        let mut messages = vec![root];
        for i in 1..35 {
            let p = f
                .run(
                    &alice,
                    Mutation::Post {
                        channel: Some("dev".into()),
                        body: format!("@bob root {i}"),
                    },
                )
                .await;
            messages.push(id(&p.result, "message_id"));
        }
        native
            .execute(
                Uuid::new_v4(),
                &bob,
                Mutation::Read {
                    message_ids: messages[..19].to_vec(),
                },
            )
            .await
            .unwrap();
        for i in 0..18 {
            let p = native
                .execute(
                    Uuid::new_v4(),
                    &bob,
                    Mutation::Reply {
                        read_message_ids: Vec::new(),
                        root_message_id: root,
                        body: format!("@alice reply {i}"),
                    },
                )
                .await
                .unwrap();
            messages.push(id(&p.result, "message_id"));
        }
        f.timeout(1).await;
        let timed = f
            .run(
                &alice,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "@bob timeout".into(),
                },
            )
            .await;
        messages.push(id(&timed.result, "message_id"));
        f.timeout(600).await;
        for i in 0..17 {
            f.run(
                &alice,
                Mutation::Reserve {
                    patterns: vec![format!("copy/group{i}/**")],
                    ttl_seconds: Some(300),
                    reason: Some("retain the lease".into()),
                },
            )
            .await;
        }
        // Materialize enough real processing expiries for multi-chunk notice
        // copying. Each acceptance is the original correlated synthetic input;
        // queued recipients behind it consume no deadline and are not expired.
        f.timeout(1).await;
        for _ in 0..20 {
            native.heartbeat(&bob).await.unwrap();
            let dispatch = native.prepare_message(&bob).await.unwrap().unwrap();
            f.clock.advance(2);
            native
                .finish_message(&bob, &dispatch, Ok(HostReceipt::default()))
                .await
                .unwrap();
        }
        f.timeout(600).await;
        let dispatch = native.prepare_wake(&bob).await.unwrap().unwrap();
        native
            .finish_wake(&bob, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap();
        native.prepare_message(&bob).await.unwrap().unwrap();
        let page = PageRequest {
            limit: Some(200),
            ..Default::default()
        };
        let mut queries = vec![
            Query::Whoami,
            Query::History {
                channel: None,
                thread: None,
                page,
            },
            Query::Events {
                channel: None,
                page,
            },
            Query::Inbox { page },
            Query::Reservations {
                owner: None,
                all: true,
                page,
            },
        ];
        for message_id in messages {
            queries.push(Query::Message {
                message_id,
                receipts: page,
            });
        }
        let high_water = f.store.snapshot().await.unwrap().dispatch_head().get();
        let mut dataset = Self {
            f,
            alice,
            bob,
            request,
            command,
            result: posted.result,
            queries,
            baseline: Vec::new(),
            high_water,
        };
        dataset.baseline = dataset.capture(&dataset.f.app).await;
        dataset
    }
    pub async fn capture(&self, app: &Application) -> Vec<Value> {
        let mut values = Vec::new();
        for query in &self.queries {
            values.push(
                app.query(Some(&self.bob), query.clone())
                    .await
                    .map(|value| serde_json::to_value(value).unwrap())
                    .unwrap(),
            );
        }
        values
    }
    pub async fn late_write(&self) {
        let p = self
            .f
            .post(&self.alice, "after the captured snapshot")
            .await;
        assert!(p.event_head.unwrap().get() > self.high_water);
    }
    pub async fn source_sequence(&self) -> i64 {
        let rows = self.f.rows("sqlite_sequence").await;
        assert_eq!(rows.len(), 1);
        rows[0].try_get("", "seq").unwrap()
    }
    pub async fn replay_source(&self) {
        let replay = self
            .f
            .app
            .execute(self.request, &self.alice, self.command.clone())
            .await
            .unwrap();
        assert_eq!(replay.result, self.result);
        assert!(replay.event_head.is_none());
    }
    pub async fn restore(&self, path: &Path, expected: &[Value]) {
        let store = Arc::new(
            Store::open(path, self.f.store.identity().clone())
                .await
                .unwrap(),
        );
        store.integrity_check().await.unwrap();
        let app = Application::new(store.clone(), self.f.clock.clone(), self.f.host.clone());
        assert_eq!(self.capture(&app).await, expected);
        let replay = app
            .execute(self.request, &self.alice, self.command.clone())
            .await
            .unwrap();
        assert_eq!(replay.result, self.result);
        assert!(replay.event_head.is_none());
        assert_eq!(self.capture(&app).await, expected);
        // Preserve the shared causal allocator independently of dense audit
        // ordinals; reopening never reuses any captured Message/Notice order.
        let p = app
            .execute(
                Uuid::new_v4(),
                &self.alice,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "independent restored write".into(),
                },
            )
            .await
            .unwrap();
        assert!(p.event_head.unwrap().get() > self.high_water);
        drop(app);
        Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
    }
}
