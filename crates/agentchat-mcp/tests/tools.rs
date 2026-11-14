//! Concrete SDK schemas, adaptation and connection invariants.
mod support;
use agentchat_protocol::operations::Target;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};
use support::*;
use uuid::Uuid;

#[tokio::test]
async fn discovery_is_inert_and_schemas_are_closed_and_concrete() {
    let backend = Arc::new(Fake::default());
    let session = Session::new(backend.clone()).await;
    let tools = session.client.peer().list_tools(None).await.unwrap().tools;
    assert_eq!(tools.len(), 23);
    for tool in &tools {
        assert!(!tool.description.as_ref().unwrap().is_empty());
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert_eq!(
            tool.output_schema.as_ref().unwrap()["additionalProperties"],
            false
        );
        assert!(
            !tool.input_schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("operation")
        );
    }
    for removed in [
        "agentchat_status",
        "agentchat_delivery_ack",
        "agentchat_ack",
        "agentchat_backup",
    ] {
        assert!(!tools.iter().any(|tool| tool.name == removed));
    }
    let schema = |name: &str| &tools.iter().find(|t| t.name == name).unwrap().input_schema;
    assert!(
        schema("agentchat_join")
            .get("required")
            .and_then(Value::as_array)
            .is_none_or(|required| !required.contains(&json!("context_id")))
    );
    assert!(
        schema("agentchat_reservations_renew")["required"]
            .as_array()
            .unwrap()
            .contains(&json!("generation"))
    );
    assert_eq!(
        schema("agentchat_read")["properties"]["message_ids"]["items"]["format"],
        "uuid"
    );
    assert_eq!(
        schema("agentchat_init")["properties"]["default_channels"]["type"],
        json!(["array", "null"])
    );
    assert!(
        schema("agentchat_init")
            .get("required")
            .and_then(Value::as_array)
            .is_none_or(|required| !required.contains(&json!("default_channels")))
    );
    assert!(
        schema("agentchat_post")
            .get("required")
            .and_then(Value::as_array)
            .is_none_or(|required| !required.contains(&json!("channel")))
    );
    assert_eq!(
        schema("agentchat_channel_list")["properties"]["state"]["$ref"],
        "#/$defs/ChannelState"
    );
    assert_eq!(
        schema("agentchat_channel_list")["$defs"]["ChannelState"]["enum"],
        json!(["active", "archived", "all"])
    );
    assert_eq!(
        schema("agentchat_channel_list")["properties"]["state"]["default"],
        "active"
    );
    assert!(
        schema("agentchat_init")["properties"]
            .get("auto_ack")
            .is_none()
    );
    assert!(backend.calls.lock().unwrap().is_empty());
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    session.close().await;
}

#[tokio::test]
async fn unbound_reads_need_explicit_path_and_mutations_need_join() {
    let backend = Arc::new(Fake::default());
    let session = Session::new(backend.clone()).await;
    let invalid = envelope(session.call("agentchat_whoami", json!({})).await);
    assert_eq!(invalid["error"]["code"], "session_required");
    let mutation = envelope(
        session
            .call(
                "agentchat_post",
                json!({"project_path":"/explicit","body":"literal"}),
            )
            .await,
    );
    assert_eq!(mutation["error"]["code"], "session_required");
    assert!(backend.calls.lock().unwrap().is_empty());
    assert!(
        envelope(
            session
                .call("agentchat_history", json!({"project_path":"/explicit"}))
                .await
        )
        .get("result")
        .is_some()
    );
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    for (tool, arguments) in [
        ("agentchat_join", json!({"context_id":"not-a-uuid"})),
        (
            "agentchat_reservations_renew",
            json!({"reservation_id":Uuid::new_v4()}),
        ),
        (
            "agentchat_whoami",
            json!({"project_path":"/explicit","operation":{}}),
        ),
        (
            "agentchat_history",
            json!({"project_path":"/explicit","after":1}),
        ),
    ] {
        let result = session.client.call_tool(params(tool, arguments)).await;
        assert!(
            match &result {
                Err(_) => true,
                Ok(value) => value.is_error == Some(true),
            },
            "{tool}: {result:?}"
        );
    }
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    session.close().await;
}

#[tokio::test]
async fn binding_is_immutable_and_own_retries_keep_the_generated_key() {
    let backend = Arc::new(Fake::default());
    backend.failures.store(2, Ordering::SeqCst);
    let session = Session::new(backend.clone()).await;
    let context = Uuid::new_v4();
    let joined = session.join(context).await;
    assert!(joined.get("result").is_some(), "{joined}");
    assert_eq!(backend.attached.load(Ordering::SeqCst), 1);
    {
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(
            calls
                .iter()
                .all(|(id, _, _)| id.to_string() == joined["request_id"])
        );
    }
    let rejected = envelope(
        session
            .call("agentchat_join", json!({"context_id":Uuid::new_v4()}))
            .await,
    );
    assert_eq!(rejected["error"]["code"], "identity_conflict");
    assert_eq!(backend.calls.lock().unwrap().len(), 3);
    session.call("agentchat_whoami", json!({})).await;
    {
        let calls = backend.calls.lock().unwrap();
        let target = &calls.last().unwrap().1;
        assert_eq!(target.context_id, Some(context));
        assert_eq!(target.expected_project_id, Some(backend.project));
    }
    session.close().await;
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_restore_never_invites_and_post_commit_failure_keeps_binding() {
    let backend = Arc::new(Fake::default());
    let server = agentchat_mcp::Server::new(backend.clone());
    let context = Uuid::new_v4();
    backend.attach_fails.store(true, Ordering::SeqCst);
    assert!(
        server
            .restore(Target {
                context_id: Some(context),
                ..Target::default()
            })
            .await
            .is_err()
    );
    assert!(backend.calls.lock().unwrap().is_empty());
    let session = Session::from_server(server).await;
    let result = session.join(context).await;
    assert_eq!(result["error"]["code"], "not_joined");
    assert_eq!(result["error"]["details"]["request_processed"], true);
    let switched = envelope(
        session
            .call("agentchat_join", json!({"context_id":Uuid::new_v4()}))
            .await,
    );
    assert_eq!(switched["error"]["code"], "identity_conflict");
    backend.attach_fails.store(false, Ordering::SeqCst);
    let retried = envelope(
        session
            .call(
                "agentchat_join",
                json!({"context_id":context,"request_id":result["request_id"]}),
            )
            .await,
    );
    assert_eq!(retried["request_id"], result["request_id"]);
    assert!(retried.get("result").is_some());
    session.close().await;
}

#[tokio::test]
async fn successful_explicit_restore_binds_without_any_join_operation() {
    let backend = Arc::new(Fake::default());
    let server = agentchat_mcp::Server::new(backend.clone());
    let context = Uuid::new_v4();
    server
        .restore(Target {
            context_id: Some(context),
            ..Target::default()
        })
        .await
        .unwrap();
    assert!(backend.calls.lock().unwrap().is_empty());
    let session = Session::from_server(server).await;
    assert!(
        envelope(session.call("agentchat_whoami", json!({})).await)
            .get("result")
            .is_some()
    );
    {
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1.context_id, Some(context));
        assert_eq!(calls[0].1.expected_project_id, Some(backend.project));
    }
    session.close().await;
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn large_complete_result_is_not_duplicated_as_escaped_text() {
    let backend = Arc::new(Fake::default());
    let body = "\"\\\nあ".repeat(70_000);
    let page = json!({"items":[{"body":body}],"next_cursor":null});
    assert!(
        serde_json::to_vec(&page).unwrap().len() < agentchat_protocol::envelope::MAX_PAGE_BYTES
    );
    *backend.result.lock().unwrap() = Some(page.clone());
    let session = Session::new(backend).await;
    let result = session
        .call("agentchat_history", json!({"project_path":"/explicit"}))
        .await;
    assert!(serde_json::to_vec(&result.content).unwrap().len() < 256);
    assert_eq!(envelope(result)["result"], page);
    session.close().await;
}

#[tokio::test]
async fn every_tool_adapts_to_the_shared_typed_operation() {
    let backend = Arc::new(Fake::default());
    let session = Session::new(backend.clone()).await;
    session.join(Uuid::new_v4()).await;
    let id = Uuid::new_v4();
    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "agentchat_init",
            json!({"scope":"directory","default_channels":[" ops ","general","ops"],"relocate":true}),
            json!({"operation":"initialize","scope":"directory","default_channels":[" ops ","general","ops"],"auto_read":null,"delivery_confirmation_timeout_seconds":null,"relocate":true,"new_project":false}),
        ),
        ("agentchat_doctor", json!({}), json!({"operation":"doctor"})),
        (
            "agentchat_leave",
            json!({}),
            json!({"operation":"mutate","command":{"operation":"leave"}}),
        ),
        (
            "agentchat_whoami",
            json!({}),
            json!({"operation":"query","query":{"operation":"whoami"}}),
        ),
        (
            "agentchat_agents_list",
            json!({"limit":2,"after":"3"}),
            json!({"operation":"query","query":{"operation":"agents","page":{"limit":2,"after":"3"}}}),
        ),
        (
            "agentchat_channel_create",
            json!({"name":"ops"}),
            json!({"operation":"mutate","command":{"operation":"channel_create","name":"ops"}}),
        ),
        (
            "agentchat_channel_join",
            json!({"name":"ops"}),
            json!({"operation":"mutate","command":{"operation":"channel_join","name":"ops"}}),
        ),
        (
            "agentchat_channel_leave",
            json!({"name":"ops"}),
            json!({"operation":"mutate","command":{"operation":"channel_leave","name":"ops"}}),
        ),
        (
            "agentchat_channel_list",
            json!({}),
            json!({"operation":"query","query":{"operation":"channels","condition":"active","page":{"limit":null,"after":"0"}}}),
        ),
        (
            "agentchat_channel_members",
            json!({"name":"dev"}),
            json!({"operation":"query","query":{"operation":"channel_members","channel":"dev","page":{"limit":null,"after":"0"}}}),
        ),
        (
            "agentchat_post",
            json!({"body":"literal $(echo no)\n`command`"}),
            json!({"operation":"mutate","command":{"operation":"post","body":"literal $(echo no)\n`command`"}}),
        ),
        (
            "agentchat_reply",
            json!({"root_message_id":id,"body":"reply"}),
            json!({"operation":"mutate","command":{"operation":"reply","root_message_id":id,"body":"reply"}}),
        ),
        (
            "agentchat_message",
            json!({"message_id":id}),
            json!({"operation":"query","query":{"operation":"message","message_id":id,"receipts":{"limit":null,"after":"0"}}}),
        ),
        (
            "agentchat_history",
            json!({"channel":"dev","thread":id}),
            json!({"operation":"query","query":{"operation":"history","channel":"dev","thread":id,"page":{"limit":null,"after":"0"}}}),
        ),
        (
            "agentchat_search",
            json!({"query":"[literal]"}),
            json!({"operation":"query","query":{"operation":"search","query":"[literal]","channel":null,"thread":null,"page":{"limit":null,"after":"0"}}}),
        ),
        (
            "agentchat_inbox",
            json!({}),
            json!({"operation":"query","query":{"operation":"inbox","page":{"limit":null,"after":"0"}}}),
        ),
        (
            "agentchat_read",
            json!({"message_ids":[id]}),
            json!({"operation":"mutate","command":{"operation":"read","message_ids":[id]}}),
        ),
        (
            "agentchat_events",
            json!({"after":"7"}),
            json!({"operation":"query","query":{"operation":"events","channel":null,"page":{"limit":null,"after":"7"}}}),
        ),
        (
            "agentchat_reserve",
            json!({"patterns":["src/**"],"ttl":60,"reason":"edit"}),
            json!({"operation":"mutate","command":{"operation":"reserve","patterns":["src/**"],"ttl_seconds":60,"reason":"edit"}}),
        ),
        (
            "agentchat_reservations_list",
            json!({"all":true,"owner":"alpha"}),
            json!({"operation":"query","query":{"operation":"reservations","owner":"alpha","all":true,"page":{"limit":null,"after":"0"}}}),
        ),
        (
            "agentchat_reservations_renew",
            json!({"reservation_id":id,"generation":5}),
            json!({"operation":"mutate","command":{"operation":"reservations_renew","reservation_id":id,"generation":5}}),
        ),
        (
            "agentchat_reservations_release",
            json!({"reservation_id":id,"generation":5}),
            json!({"operation":"mutate","command":{"operation":"reservations_release","reservation_id":id,"generation":5}}),
        ),
    ];
    for (name, arguments, expected) in cases {
        let result = envelope(session.call(name, arguments).await);
        assert!(result.get("result").is_some(), "{name}: {result}");
        let actual =
            serde_json::to_value(&backend.calls.lock().unwrap().last().unwrap().2).unwrap();
        assert_eq!(actual, expected, "{name}");
    }
    session.close().await;
}

#[tokio::test]
async fn reply_omitted_null_and_empty_read_sets_have_one_canonical_shared_operation() {
    let backend = Arc::new(Fake::default());
    let session = Session::new(backend.clone()).await;
    session.join(Uuid::new_v4()).await;
    let root = Uuid::new_v4();
    let mut commands = Vec::new();
    for reads in [None, Some(Value::Null), Some(json!([]))] {
        let mut args = json!({"root_message_id":root,"body":"literal"});
        if let Some(reads) = reads {
            args["read_message_ids"] = reads;
        }
        let result = envelope(session.call("agentchat_reply", args).await);
        assert!(result.get("result").is_some(), "{result}");
        commands
            .push(serde_json::to_value(&backend.calls.lock().unwrap().last().unwrap().2).unwrap());
    }
    assert_eq!(commands[0], commands[1]);
    assert_eq!(commands[1], commands[2]);
    session.close().await;
}
