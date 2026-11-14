//! Online snapshot copying preserves domain/history and never overwrites a file.

mod support;
use agentchat_core::{
    ErrorCode,
    application::{Application, Mutation},
    ports::Storage,
    read::Query,
};
use agentchat_store::Store;
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, ColumnDef, Expr, ExprTrait, Query as SqlQuery, Table},
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::Arc,
};
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn backup_preserves_messages_inboxes_leases_receipts_and_successful_retry_history() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let gate = support::native::Gate::new(&f);
    gate.bind(&b);
    let native = gate.app(&f);
    native.start_launch(&b, Some("bob")).await.unwrap();
    let key = Uuid::new_v4();
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: "preserve @bob".into(),
    };
    let posted = f.app.execute(key, &a, command.clone()).await.unwrap();
    let root = id(&posted.result, "message_id");
    native
        .execute(
            Uuid::new_v4(),
            &b,
            Mutation::Reply {
                read_message_ids: Vec::new(),
                root_message_id: root,
                body: "reply".into(),
            },
        )
        .await
        .unwrap();
    native
        .execute(
            Uuid::new_v4(),
            &b,
            Mutation::Read {
                message_ids: vec![root],
            },
        )
        .await
        .unwrap();
    let dispatch = native.prepare_message(&b).await.unwrap().unwrap();
    native
        .finish_message(
            &b,
            &dispatch,
            Ok(agentchat_core::delivery::HostReceipt::default()),
        )
        .await
        .unwrap();
    f.run(
        &a,
        Mutation::Reserve {
            patterns: vec!["src/**".into()],
            ttl_seconds: None,
            reason: Some("copy this group".into()),
        },
    )
    .await;
    let destination = f.root.path().join("backup.sqlite3");
    // Audit ordinals are dense private integrity keys; causal sequence gaps
    // belong to independent Dispatches, not an injected sqlite_sequence jump.
    let next = f.store.snapshot().await.unwrap().dispatch_head().get();
    let audit_counters = f.rows("sqlite_sequence").await;
    let audit_counter = audit_counters[0].try_get::<i64>("", "seq").unwrap();
    let next_sequence = f.rows("project_meta").await[0]
        .try_get::<i64>("", "next_sequence")
        .unwrap();
    let report = f.store.backup(&destination).await.unwrap();
    assert_eq!(report.head, f.app.event_head().await.unwrap());
    assert!(report.rows > 10);
    assert_eq!(
        fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!destination.with_file_name("backup.sqlite3-wal").exists());
    let restored = Arc::new(
        Store::open(&destination, f.store.identity().clone())
            .await
            .unwrap(),
    );
    let inspect = support::upgrade::inspect(&destination).await;
    assert_eq!(
        support::upgrade::rows(&inspect, "project_meta").await[0]
            .try_get::<i64>("", "next_sequence")
            .unwrap(),
        next_sequence
    );
    assert_eq!(
        support::upgrade::rows(&inspect, "sqlite_sequence").await[0]
            .try_get::<i64>("", "seq")
            .unwrap(),
        audit_counter
    );
    inspect.close().await.unwrap();
    restored.integrity_check().await.unwrap();
    let app = Application::new(restored.clone(), f.clock.clone(), f.host.clone());
    for query in [
        Query::Whoami,
        Query::History {
            channel: None,
            thread: None,
            page: Default::default(),
        },
        Query::Message {
            message_id: root,
            receipts: Default::default(),
        },
        Query::Inbox {
            page: Default::default(),
        },
        Query::Reservations {
            owner: None,
            all: true,
            page: Default::default(),
        },
        Query::Events {
            channel: None,
            page: Default::default(),
        },
    ] {
        assert_eq!(
            app.query(Some(&b), query.clone())
                .await
                .map(|value| serde_json::to_value(value).unwrap())
                .unwrap(),
            f.query(Some(&b), query).await
        );
    }
    assert_eq!(
        app.execute(key, &a, command).await.unwrap().result,
        posted.result
    );
    assert!(
        app.execute(
            Uuid::new_v4(),
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "after backup".into()
            }
        )
        .await
        .unwrap()
        .event_head
        .unwrap()
        .get()
            > next
    );
    assert!(!fs::read_dir(f.root.path()).unwrap().any(|p| {
        p.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".agentchat-backup-")
    }));
}

#[tokio::test]
async fn backup_preserves_ordered_nondev_policy_and_exact_memberships() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let native = f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let common = f
        .app
        .execute_actor(
            Uuid::new_v4(),
            &agentchat_core::model::OperationActor::User,
            Mutation::Join { name: None },
        )
        .await
        .unwrap();
    let user = id(&common.result, "actor_id");
    f.app
        .initialize_with_defaults(Some(vec!["ops".into(), "planning".into()]))
        .await
        .unwrap();
    let before_repeat = f.counts().await;
    let repeated = f
        .app
        .initialize_with_defaults(Some(vec!["ops".into(), "planning".into()]))
        .await
        .unwrap();
    assert_eq!(repeated.result["changed"], false);
    assert_eq!(
        f.counts().await,
        before_repeat,
        "reapplying defaults creates no membership or notification facts"
    );
    let mut tx = f.store.begin().await.unwrap();
    for name in ["ops", "planning"] {
        let channel = tx.channel(name).await.unwrap().unwrap();
        for actor in [native, user] {
            assert!(
                tx.membership_boundary(channel.id, actor)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
    }
    tx.rollback().await.unwrap();
    assert!(
        !f.rows("deliveries")
            .await
            .iter()
            .any(|r| r.try_get::<String>("", "recipient_id").unwrap() == user.to_string())
    );
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "optional".into(),
        },
    )
    .await;
    f.run(
        &a,
        Mutation::ChannelJoin {
            name: "optional".into(),
        },
    )
    .await;
    let status = f.health().await;
    let channels = ["dev", "ops", "planning", "optional"];
    let mut memberships = Vec::new();
    for channel in channels {
        memberships.push((
            channel,
            f.query(
                None,
                Query::ChannelMembers {
                    channel: channel.into(),
                    page: Default::default(),
                },
            )
            .await,
        ));
    }

    let destination = f.root.path().join("policy-backup.sqlite3");
    f.store.backup(&destination).await.unwrap();
    let restored = Arc::new(
        Store::open(&destination, f.store.identity().clone())
            .await
            .unwrap(),
    );
    let app = Application::new(restored, f.clock.clone(), f.host.clone());
    let restored_status = app
        .health()
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert_eq!(restored_status["channel_policy"], status["channel_policy"]);
    assert_eq!(
        restored_status["channel_policy"]["ordered_defaults"],
        serde_json::json!(["ops", "planning"])
    );
    for (channel, expected) in memberships {
        assert_eq!(
            app.query(
                None,
                Query::ChannelMembers {
                    channel: channel.into(),
                    page: Default::default(),
                },
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn concurrent_writes_leave_a_self_consistent_backup_at_the_reported_head() {
    let f = Fixture::new().await;
    let context = f.context();
    f.join(&context, "alice").await;
    for _ in 0..32 {
        f.post(&context, "before").await;
    }
    let app = Application::new(f.store.clone(), f.clock.clone(), f.host.clone());
    let actor = context.clone();
    let writer = tokio::spawn(async move {
        for n in 0..40 {
            app.execute(
                Uuid::new_v4(),
                &actor,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: format!("during {n}"),
                },
            )
            .await
            .unwrap();
        }
    });
    let destination = f.root.path().join("online.sqlite3");
    let report = f.store.backup(&destination).await.unwrap();
    writer.await.unwrap();
    let restored = Arc::new(
        Store::open(&destination, f.store.identity().clone())
            .await
            .unwrap(),
    );
    restored.integrity_check().await.unwrap();
    let app = Application::new(restored, f.clock.clone(), f.host.clone());
    let status = app
        .health()
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert_eq!(status["head"], serde_json::to_value(report.head).unwrap());
    let count = status["statistics"]["messages"].as_u64().unwrap();
    assert!((32..=72).contains(&count));
    let events = app
        .query(
            None,
            Query::Events {
                channel: None,
                page: agentchat_core::read::PageRequest {
                    limit: Some(200),
                    ..Default::default()
                },
            },
        )
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert_eq!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "message.created")
            .count() as u64,
        count
    );
    assert_eq!(f.health().await["statistics"]["messages"], 72);
}

#[tokio::test]
async fn existing_destinations_and_noncanonical_schema_are_refused_without_replacement() {
    let f = Fixture::new().await;
    let destination = f.root.path().join("existing.sqlite3");
    fs::write(&destination, b"foreign backup").unwrap();
    assert_eq!(
        f.store.backup(&destination).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(&destination).unwrap(), b"foreign backup");
    let link = f.root.path().join("link.sqlite3");
    symlink(&destination, &link).unwrap();
    assert_eq!(
        f.store.backup(&link).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    f.inspect
        .execute(
            &Table::create()
                .table(Alias::new("foreign_table"))
                .col(ColumnDef::new(Alias::new("id")).integer())
                .to_owned(),
        )
        .await
        .unwrap();
    let fresh = f.root.path().join("refused.sqlite3");
    assert_eq!(
        f.store.backup(&fresh).await.unwrap_err().code,
        ErrorCode::IntegrityError
    );
    assert!(!fresh.exists());
    assert_eq!(fs::read(destination).unwrap(), b"foreign backup");
    assert_eq!(f.count("foreign_table").await, 0);
}

#[tokio::test]
async fn metadata_changed_after_open_is_checked_again_inside_the_backup_snapshot() {
    let f = Fixture::new().await;
    f.inspect
        .execute(
            &SqlQuery::update()
                .table(Alias::new("project_meta"))
                .value(
                    Alias::new("schema_version"),
                    agentchat_store::SCHEMA_VERSION + 1,
                )
                .and_where(Expr::col(Alias::new("singleton")).eq(1))
                .to_owned(),
        )
        .await
        .unwrap();
    let destination = f.root.path().join("future.sqlite3");
    assert_eq!(
        f.store.backup(&destination).await.unwrap_err().code,
        ErrorCode::UnsupportedSchema
    );
    assert!(!destination.exists());
    assert_eq!(
        f.rows("project_meta").await[0]
            .try_get::<i32>("", "schema_version")
            .unwrap(),
        agentchat_store::SCHEMA_VERSION + 1
    );
}

#[tokio::test]
async fn backup_preserves_author_leave_fence_without_erasing_recipient_timeout_history() {
    let f = Fixture::new().await;
    let author = f.context();
    let recipient = f.context();
    let author_id = f.join(&author, "alice").await;
    let gate = support::native::Gate::new(&f);
    gate.bind(&recipient);
    let native = gate.app(&f);
    native.start_launch(&recipient, Some("bob")).await.unwrap();
    f.timeout(1).await;
    let old = id(
        &f.post(&author, "@bob original before author Leave")
            .await
            .result,
        "message_id",
    );
    let original = native.prepare_message(&recipient).await.unwrap().unwrap();
    f.run(&author, Mutation::Leave).await;
    let fence = f
        .rows("actors")
        .await
        .into_iter()
        .find(|row| row.try_get::<String>("", "id").unwrap() == author_id.to_string())
        .unwrap()
        .try_get::<i64>("", "timeout_notice_after_seq")
        .unwrap();
    assert!(fence >= original.input.seq.get());
    f.join(&author, "alice").await;
    let retained = f
        .rows("actors")
        .await
        .into_iter()
        .find(|row| row.try_get::<String>("", "id").unwrap() == author_id.to_string())
        .unwrap()
        .try_get::<i64>("", "timeout_notice_after_seq")
        .unwrap();
    assert_eq!(
        retained, fence,
        "ordinary rejoin must not clear explicit cancellation"
    );
    let new = id(
        &f.post(&author, "@bob fresh after author rejoin")
            .await
            .result,
        "message_id",
    );
    let destination = f.root.path().join("author-fence.sqlite3");
    f.store.backup(&destination).await.unwrap();
    let restored = Arc::new(
        Store::open(&destination, f.store.identity().clone())
            .await
            .unwrap(),
    );
    let inspect = support::upgrade::inspect(&destination).await;
    let restored_fence = support::upgrade::rows(&inspect, "actors")
        .await
        .into_iter()
        .find(|row| row.try_get::<String>("", "id").unwrap() == author_id.to_string())
        .unwrap()
        .try_get::<i64>("", "timeout_notice_after_seq")
        .unwrap();
    assert_eq!(restored_fence, fence);
    let restored_native = Application::new(restored.clone(), f.clock.clone(), gate);
    f.clock.advance(2);
    restored_native
        .finish_message(
            &recipient,
            &original,
            Ok(agentchat_core::delivery::HostReceipt::default()),
        )
        .await
        .unwrap();
    let mut tx = restored.begin().await.unwrap();
    let late = tx
        .delivery(original.input.delivery_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        late.state,
        agentchat_core::model::DeliveryState::DeliveredLate
    );
    assert!(late.history.timed_out_at.is_some() && late.history.delivered_at.is_some());
    assert!(tx.delivery_timeout_notice(old).await.unwrap().is_none());
    tx.rollback().await.unwrap();
    let mut fresh = None;
    for _ in 0..200 {
        if let Some(body) = restored_native.prepare_message(&recipient).await.unwrap() {
            fresh = Some(body);
            break;
        }
        let notice = restored_native
            .prepare_native(&recipient)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            &notice,
            agentchat_core::delivery::NativeDispatch::Notice(_)
        ));
        assert!(
            restored_native
                .finish_native(
                    &recipient,
                    &notice,
                    Ok(agentchat_core::delivery::MessageProgress::Accepted(
                        agentchat_core::delivery::HostReceipt::default()
                    ))
                )
                .await
                .unwrap()
        );
    }
    let fresh = fresh.expect("bounded lifecycle notices precede the new body");
    assert_eq!(fresh.input.message_id, new);
    f.clock.advance(2);
    restored_native
        .finish_message(
            &recipient,
            &fresh,
            Ok(agentchat_core::delivery::HostReceipt::default()),
        )
        .await
        .unwrap();
    let notices = support::upgrade::rows(&inspect, "delivery_timeout_notices").await;
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].try_get::<String>("", "message_id").unwrap(),
        new.to_string()
    );
    restored.integrity_check().await.unwrap();
    inspect.close().await.unwrap();
}
