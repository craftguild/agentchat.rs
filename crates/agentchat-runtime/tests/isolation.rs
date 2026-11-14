//! Normal discovered namespaces cannot retrieve another project's domain records.

mod support;

use agentchat_core::{
    ErrorCode,
    application::{Application, Mutation},
    model::{Context, HostKind},
    read::{PageRequest, Query},
    time::SystemClock,
};
use std::{fs, sync::Arc};
use support::{Fixture, SyntheticHost};
use uuid::Uuid;

#[tokio::test]
async fn isolated_discovery_routes_messages_agents_inboxes_and_reservations_to_one_uuid() {
    let f = Fixture::new();
    let second = f.root.path().join("second");
    fs::create_dir(&second).unwrap();
    let first = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let other = f.manager.initialize(&second, f.request()).await.unwrap();
    assert_ne!(first.project.identity(), other.project.identity());
    let discovered_a = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    let discovered_b = f.manager.discover(&second).await.unwrap().unwrap();
    let store_a = f.manager.open(&discovered_a).await.unwrap();
    let store_b = f.manager.open(&discovered_b).await.unwrap();
    let host = Arc::new(SyntheticHost);
    let app_a = Application::new(store_a, Arc::new(SystemClock), host.clone());
    let app_b = Application::new(store_b, Arc::new(SystemClock), host);
    let a = Context {
        context_id: Uuid::new_v4(),
        project_id: first.project.identity().id,
        host: HostKind::Codex,
        host_session_id: Uuid::new_v4().to_string(),
        host_version: "fixture-only".into(),
        os_profile: "synthetic-not-admitted".into(),
        working_directory: f.workspace.clone(),
        incarnation: Uuid::new_v4(),
    };
    let joined = app_a
        .execute(
            Uuid::new_v4(),
            &a,
            Mutation::Join {
                name: Some("alice".into()),
            },
        )
        .await
        .unwrap();
    let post = app_a
        .execute(
            Uuid::new_v4(),
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "private first project history".into(),
            },
        )
        .await
        .unwrap();
    let message = Uuid::parse_str(post.result["message_id"].as_str().unwrap()).unwrap();
    app_a
        .execute(
            Uuid::new_v4(),
            &a,
            Mutation::Reserve {
                patterns: vec!["src/**".into()],
                ttl_seconds: None,
                reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        app_b
            .query(
                None,
                Query::Message {
                    message_id: message,
                    receipts: PageRequest::default()
                }
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    for query in [
        Query::History {
            channel: None,
            thread: None,
            page: PageRequest::default(),
        },
        Query::Agents {
            page: PageRequest::default(),
        },
        Query::Reservations {
            owner: None,
            all: true,
            page: PageRequest::default(),
        },
    ] {
        assert!(
            app_b
                .query(None, query)
                .await
                .map(|value| serde_json::to_value(value).unwrap())
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        app_b
            .query(
                Some(&a),
                Query::Inbox {
                    page: PageRequest::default()
                }
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::ProjectBindingMismatch
    );
    let mut b = a.clone();
    b.project_id = other.project.identity().id;
    b.context_id = Uuid::new_v4();
    b.incarnation = Uuid::new_v4();
    b.host_session_id = Uuid::new_v4().to_string();
    b.working_directory = second.canonicalize().unwrap();
    let independent = app_b
        .execute(
            Uuid::new_v4(),
            &b,
            Mutation::Join {
                name: Some("alice".into()),
            },
        )
        .await
        .unwrap();
    assert_ne!(independent.result["agent_id"], joined.result["agent_id"]);
    // Names and logical paths are project-local; a reservation in A cannot block B.
    app_b
        .execute(
            Uuid::new_v4(),
            &b,
            Mutation::Reserve {
                patterns: vec!["src/**".into()],
                ttl_seconds: None,
                reason: None,
            },
        )
        .await
        .unwrap();
    let before = app_a
        .query(
            None,
            Query::Events {
                channel: None,
                page: PageRequest::default(),
            },
        )
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    let mut replace = f.request();
    replace.new_project = true;
    let replacement = f.manager.initialize(&f.workspace, replace).await.unwrap();
    assert_ne!(replacement.project.identity(), first.project.identity());
    let current = f
        .manager
        .open(&f.manager.discover(&f.workspace).await.unwrap().unwrap())
        .await
        .unwrap();
    let mut snapshot = agentchat_core::ports::Storage::snapshot(current.as_ref())
        .await
        .unwrap();
    assert_eq!(snapshot.statistics().await.unwrap().messages, 0);
    assert_eq!(
        app_a
            .query(
                None,
                Query::Events {
                    channel: None,
                    page: PageRequest::default()
                }
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap(),
        before
    );
    assert!(first.project.database_path().exists());
}
