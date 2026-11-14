//! Canonical actor wire references are closed and removed interfaces have no aliases.

use agentchat_core::application::Mutation;
use agentchat_protocol::{
    operations::Operation,
    rpc::{ActorReference, Call, Request},
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn actor_references_distinguish_anonymous_selection_from_observation_and_reject_fake_facts() {
    let id = Uuid::new_v4();
    for actor in [
        ActorReference::Native { context_id: id },
        ActorReference::User {},
    ] {
        let raw = serde_json::to_value(actor).unwrap();
        assert_eq!(
            serde_json::from_value::<ActorReference>(raw).unwrap(),
            actor
        );
    }
    for raw in [
        json!({"kind":"user","context_id":id}),
        json!({"kind":"user","host":"codex"}),
        json!({"kind":"native","context_id":id,"host":"codex"}),
        json!({"kind":"native"}),
    ] {
        assert!(serde_json::from_value::<ActorReference>(raw).is_err());
    }
    let native = Call::Mutate {
        request_id: id,
        actor: ActorReference::Native { context_id: id },
        command: Mutation::Leave,
    };
    let user = Call::Mutate {
        request_id: id,
        actor: ActorReference::User {},
        command: Mutation::Leave,
    };
    assert_eq!(native.context_id(), Some(id));
    assert!(native.grouped_mutation());
    assert!(user.selects_user());
    assert!(!user.grouped_mutation());
    assert_eq!(user.context_id(), None);
    let observer = Call::Query {
        actor: None,
        query: agentchat_core::read::Query::History {
            channel: None,
            thread: None,
            page: Default::default(),
        },
    };
    assert!(!observer.selects_user());
    assert_eq!(
        Request::new(id, &user)
            .unwrap()
            .call()
            .unwrap()
            .context_id(),
        None
    );
}

#[test]
fn removed_backup_status_and_generic_subscription_inputs_are_not_compatibility_routes() {
    let id = Uuid::new_v4();
    for removed in [
        json!({"operation":"delivery_ack","delivery_ids":[id]}),
        json!({"operation":"ack","delivery_ids":[id]}),
        json!({"operation":"post","body":"literal","read_timeout_seconds":1}),
        json!({"operation":"reply","root_message_id":id,"body":"literal","read_timeout_seconds":1}),
    ] {
        assert!(serde_json::from_value::<Mutation>(removed).is_err());
    }
    assert!(!Call::known("events.subscribe"));
    assert!(Call::known("viewer.subscribe"));
    assert!(Call::known("observer.attach"));
    assert!(Call::known("native.binding"));
    assert!(Call::known("internal.health"));
    assert!(
        serde_json::from_value::<Operation>(
            json!({"operation":"backup","destination":"copy.sqlite3"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<Operation>(
            json!({"operation":"query","query":{"operation":"status"}})
        )
        .is_err()
    );
}

#[test]
fn initialization_rejects_removed_ack_policy() {
    let value = json!({"operation":"initialize", "scope":null,"default_channels":null,"auto_ack":true,"auto_read":true,"delivery_confirmation_timeout_seconds":3600,"relocate":false,"new_project":false});
    assert!(serde_json::from_value::<agentchat_protocol::operations::Operation>(value).is_err());
}
