//! Explicit user RPC routing cannot borrow native binding or observer authority.
mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    read::{PageRequest, Query, requests},
};
use agentchat_protocol::rpc::{ActorReference, Call, Client, success};
use serde_json::Value;
use support::runtime::{Running, join, mutate, options};
use uuid::Uuid;

async fn user(client: &mut Client, command: Mutation) -> Value {
    let id = Uuid::new_v4();
    success(
        client
            .call(
                id,
                Call::Mutate {
                    request_id: id,
                    actor: ActorReference::User {},
                    command,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap()
}
fn history() -> Query {
    Query::History {
        channel: None,
        thread: None,
        page: PageRequest::default(),
    }
}

#[tokio::test]
async fn native_bound_connections_cannot_switch_to_user_and_user_lookup_is_inert() {
    let r = Running::new(options()).await;
    let context = r.context("native-bound").await;
    let mut native = r.client().await;
    join(&mut native, &context, "native").await;
    let before = native.health().await.unwrap().application;
    let id = Uuid::new_v4();
    let error = native
        .call(
            id,
            Call::Mutate {
                request_id: id,
                actor: ActorReference::User {},
                command: Mutation::Join { name: None },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdentityConflict);
    let error = native
        .query_actor(Uuid::new_v4(), Some(ActorReference::User {}), history())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdentityConflict);
    let mut selected = r.client().await;
    let lookup = success(
        selected
            .read_actor(
                Uuid::new_v4(),
                Some(ActorReference::User {}),
                requests::Whoami,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(lookup.application.actor.is_none());
    assert!(lookup.application.context_id.is_none());
    assert!(lookup.application.native_delivery.is_none());
    assert_eq!(
        lookup.application.participation,
        agentchat_core::read::Participation::Unjoined
    );
    assert_eq!(native.health().await.unwrap().application, before);
    assert_eq!(
        selected
            .query_actor(Uuid::new_v4(), Some(ActorReference::User {}), history())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotJoined
    );
    assert!(
        selected
            .query_actor(Uuid::new_v4(), None, history())
            .await
            .is_ok()
    );
    r.stop().await;
}

#[tokio::test]
async fn user_calls_do_not_install_a_native_socket_binding_or_escape_membership() {
    let r = Running::new(options()).await;
    let context = r.context("author").await;
    let mut native = r.client().await;
    join(&mut native, &context, "author").await;
    mutate(
        &mut native,
        &context,
        Mutation::Post {
            channel: None,
            body: "retained observer history".into(),
        },
    )
    .await;
    let mut selected = r.client().await;
    let joined = user(&mut selected, Mutation::Join { name: None }).await;
    assert_eq!(joined["kind"], "user");
    let mut other = r.client().await;
    let again = user(&mut other, Mutation::Join { name: None }).await;
    assert_eq!(joined["actor_id"], again["actor_id"]);
    user(&mut selected, Mutation::Leave).await;
    assert_eq!(
        other
            .query_actor(Uuid::new_v4(), Some(ActorReference::User {}), history())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotJoined
    );
    let observed = success(
        other
            .query_actor(Uuid::new_v4(), None, history())
            .await
            .unwrap(),
    )
    .unwrap();
    let agentchat_core::read::QueryResult::History(page) = observed else {
        panic!("typed history")
    };
    assert_eq!(page.items.len(), 1);
    let health = selected.health().await.unwrap();
    assert_eq!(health.application.statistics.agents, 1);
    assert_eq!(health.application.statistics.joined, 1);
    // A pure user caller has no native binding. The native private read can
    // still select a real test-owned descriptor without a false identity switch.
    assert!(
        selected
            .native_binding(context.context_id)
            .await
            .unwrap()
            .agent
            .is_some()
    );
    r.stop().await;
}
