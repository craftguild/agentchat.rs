//! Current-format reopen preserves native actor UUIDs, authorship, ownership and reads.

mod support;

use agentchat_core::{
    application::{Application, Mutation},
    model::HostKind,
    ports::Storage,
    read::{PageRequest, Query, QueryResult},
};
use agentchat_store::Store;
use std::sync::Arc;
use support::Fixture;
use uuid::Uuid;

#[tokio::test]
async fn native_actor_uuids_authorship_ownership_and_first_reads_survive_current_reopen() {
    let f = Fixture::new().await;
    let codex = f.context();
    let mut claude = f.context();
    claude.host = HostKind::Claude;
    claude.host_session_id = codex.host_session_id.clone();
    let a_id = f.join(&codex, "native-a").await;
    let b_id = f.join(&claude, "native-b").await;
    assert_ne!(a_id, b_id);
    let posted = f
        .run(
            &codex,
            Mutation::Post {
                channel: None,
                body: "retained complete history".into(),
            },
        )
        .await;
    let message = Uuid::parse_str(posted.result["message_id"].as_str().unwrap()).unwrap();
    f.run(
        &claude,
        Mutation::Read {
            message_ids: vec![message],
        },
    )
    .await;
    let lease = f
        .run(
            &codex,
            Mutation::Reserve {
                patterns: vec!["lib/**".into()],
                ttl_seconds: None,
                reason: None,
            },
        )
        .await;
    let first = f
        .store
        .snapshot()
        .await
        .unwrap()
        .receipt(message, b_id)
        .await
        .unwrap()
        .unwrap()
        .first_read_at;
    let path = f.root.path().join("agentchat.sqlite3");
    let store = Arc::new(
        Store::open(&path, f.store.identity().clone())
            .await
            .unwrap(),
    );
    let mut snapshot = store.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .conversation(HostKind::Codex, &codex.host_session_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        a_id
    );
    assert_eq!(
        snapshot
            .conversation(HostKind::Claude, &claude.host_session_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        b_id
    );
    assert!(snapshot.user().await.unwrap().is_none());
    assert_eq!(
        snapshot
            .receipt(message, b_id)
            .await
            .unwrap()
            .unwrap()
            .first_read_at,
        first
    );
    drop(snapshot);
    let app = Application::new(store.clone(), f.clock.clone(), f.host.clone());
    let QueryResult::Message(detail) = app
        .query(
            None,
            Query::Message {
                message_id: message,
                receipts: PageRequest::default(),
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(detail.message.author.actor_id, a_id);
    assert_eq!(detail.receipts.items[0].recipient.actor_id, b_id);
    assert_eq!(detail.receipts.items[0].reads[0].at, first);
    let QueryResult::Reservations(groups) = app
        .query(
            None,
            Query::Reservations {
                owner: None,
                all: false,
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(groups.items[0].owner.actor_id, a_id);
    assert_eq!(
        groups.items[0].reservation_id.to_string(),
        lease.result["reservation_id"].as_str().unwrap()
    );
    assert!(support::upgrade::backups(&f).is_empty());
    store.integrity_check().await.unwrap();
}
