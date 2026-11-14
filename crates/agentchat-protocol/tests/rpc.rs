//! Wire discriminants reject ambiguous outcomes and retain standard ID forms.

use agentchat_protocol::{
    envelope::Envelope,
    operations::Operation,
    rpc::{Call, Id, Reply, Request},
};
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn absent_notification_id_is_distinct_from_null_and_numeric_ids() {
    for (id, expected) in [
        (json!(null), Id::Null),
        (json!(12), Id::Number(12.into())),
        (json!("12"), Id::Text("12".into())),
    ] {
        let request: Request =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":id,"method":"x"})).unwrap();
        assert_eq!(request.id, Some(expected));
    }
    let notification: Request =
        serde_json::from_value(json!({"jsonrpc":"2.0","method":"x"})).unwrap();
    assert!(notification.id.is_none());
    assert!(
        serde_json::from_value::<Request>(json!({"jsonrpc":"2.0","method":"x","id":true})).is_err()
    );
}
#[test]
fn response_and_envelope_accept_exactly_one_outcome() {
    let error = agentchat_core::Error::new(agentchat_core::ErrorCode::InvalidArgument, "fixture");
    let envelope = Envelope::new(Uuid::new_v4(), Some(Uuid::new_v4()), Ok(json!({"ok":true})));
    let mut encoded = serde_json::to_value(&envelope).unwrap();
    assert!(serde_json::from_value::<Envelope<Value>>(encoded.clone()).is_ok());
    encoded["error"] = serde_json::to_value(&error).unwrap();
    assert!(serde_json::from_value::<Envelope<Value>>(encoded).is_err());
    let reply = json!({"jsonrpc":"2.0","id":1,"result":envelope,"error":{"code":-32000,"message":"fixture","data":error}});
    assert!(serde_json::from_value::<Reply>(reply).is_err());
}

#[test]
fn setup_and_post_wire_preserve_omitted_policy_intent() {
    let omitted: Operation = serde_json::from_value(json!({
        "operation":"initialize",
        "scope":null,
        "relocate":false,
        "new_project":false
    }))
    .unwrap();
    assert!(matches!(
        omitted,
        Operation::Initialize {
            default_channels: None,
            ..
        }
    ));

    let supplied: Operation = serde_json::from_value(json!({
        "operation":"initialize",
        "scope":null,
        "default_channels":[" ops ","general","ops"],
        "relocate":false,
        "new_project":false
    }))
    .unwrap();
    assert!(matches!(
        supplied,
        Operation::Initialize {
            default_channels: Some(ref channels),
            ..
        } if channels == &[" ops ", "general", "ops"]
    ));

    let post: Operation = serde_json::from_value(json!({
        "operation":"mutate",
        "command":{"operation":"post","body":"omitted channel"}
    }))
    .unwrap();
    assert!(matches!(
        post,
        Operation::Mutate {
            command: agentchat_core::application::Mutation::Post { channel: None, .. }
        }
    ));
}

#[test]
fn delivery_check_wire_retains_the_exact_bounded_hint_references() {
    let context_id = Uuid::new_v4();
    let batch_id = Uuid::new_v4();
    let token = Uuid::new_v4();
    let delivery_ids = vec![Uuid::new_v4(), Uuid::new_v4()];
    let call = Call::CheckDelivery {
        context_id,
        batch_id,
        token,
        delivery_ids: delivery_ids.clone(),
    };
    let encoded = serde_json::to_value(&call).unwrap();
    assert_eq!(encoded["method"], "delivery.check.v2");
    assert_eq!(encoded["params"]["delivery_ids"], json!(delivery_ids));
    assert!(matches!(
        serde_json::from_value::<Call>(encoded).unwrap(),
        Call::CheckDelivery {
            context_id: decoded_context,
            batch_id: decoded_batch,
            token: decoded_token,
            delivery_ids: decoded_deliveries,
        } if decoded_context == context_id
            && decoded_batch == batch_id
            && decoded_token == token
            && decoded_deliveries == delivery_ids
    ));
    assert!(!Call::known("delivery.check"));
    assert!(Call::known("delivery.check.v2"));
    let mut obsolete = serde_json::to_value(call).unwrap();
    obsolete["method"] = json!("delivery.check");
    assert!(serde_json::from_value::<Call>(obsolete).is_err());
}

#[test]
fn message_check_wire_rejects_the_pre_scope_method_name() {
    let encoded = serde_json::to_value(Call::CheckMessage {
        context_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        delivery_id: Uuid::new_v4(),
        token: Uuid::new_v4(),
    })
    .unwrap();
    assert_eq!(encoded["method"], "message.check.v2");
    assert!(!Call::known("message.check"));
    assert!(Call::known("message.check.v2"));
    assert!(
        serde_json::from_value::<Call>(json!({
            "method":"message.check",
            "params":encoded["params"].clone()
        }))
        .is_err()
    );
}

#[test]
fn native_check_wire_preserves_exact_scope_without_replacing_message_checks() {
    let context_id = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    let token = Uuid::new_v4();
    let call = Call::CheckNative {
        context_id,
        generation,
        delivery_id,
        token,
    };
    let encoded = serde_json::to_value(&call).unwrap();
    assert_eq!(encoded["method"], "native.check.v1");
    assert_eq!(call.context_id(), Some(context_id));
    assert!(Call::known("native.check.v1"));
    assert!(matches!(
        serde_json::from_value::<Call>(encoded.clone()).unwrap(),
        Call::CheckNative {
            context_id: context,
            generation: launch,
            delivery_id: delivery,
            token: attempt,
        } if (context, launch, delivery, attempt) == (context_id, generation, delivery_id, token)
    ));
    let mut invalid = encoded;
    invalid["params"]["accepted"] = json!(true);
    assert!(serde_json::from_value::<Call>(invalid).is_err());
    assert!(Call::known("message.check.v2"));
}

#[test]
fn managed_activation_wire_retains_semantic_retry_and_typed_cause() {
    let request_id = Uuid::new_v4();
    let context_id = Uuid::new_v4();
    let call = Call::ActivateManaged {
        request_id,
        context_id,
        cause: agentchat_core::application::ManagedActivationCause::Initial,
        name: Some("managed".into()),
    };
    let encoded = serde_json::to_value(&call).unwrap();
    assert_eq!(encoded["method"], "launch.activate_managed.v1");
    assert_eq!(call.result_key(Uuid::new_v4()), request_id);
    assert_eq!(call.context_id(), Some(context_id));
    assert!(Call::known("launch.activate_managed.v1"));
    assert!(matches!(
        serde_json::from_value::<Call>(encoded).unwrap(),
        Call::ActivateManaged {
            request_id: decoded_request,
            context_id: decoded_context,
            cause: agentchat_core::application::ManagedActivationCause::Initial,
            name: Some(name),
        } if decoded_request == request_id && decoded_context == context_id && name == "managed"
    ));
}
