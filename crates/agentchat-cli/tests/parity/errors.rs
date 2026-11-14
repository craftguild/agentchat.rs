//! Domain failures preserve data; transport syntax errors keep their own format.
use super::rig::Rig;
use agentchat_core::ErrorCode as E;
use serde_json::{Value, json};
use uuid::Uuid;

async fn state(r: &mut Rig) -> (Value, Value) {
    let status = r
        .pair(Some(0), "agentchat_whoami", json!({}), &["whoami"])
        .await;
    let leases = r
        .pair(
            Some(0),
            "agentchat_reservations_list",
            json!({"all":true}),
            &["reservations", "list", "--all"],
        )
        .await;
    let mut status = status["result"].clone();
    // Execution counters include failed requests and successful-key replays.
    // Validate their bounds, then compare every retained/business status field.
    let groups = status["runtime"]
        .as_object_mut()
        .unwrap()
        .remove("mutation_groups")
        .unwrap();
    assert_eq!(
        groups["limit"],
        agentchat_core::application::MAX_MUTATION_GROUP
    );
    assert!(groups["largest"].as_u64().unwrap() <= groups["limit"].as_u64().unwrap());
    assert!(groups["batched_groups"].as_u64().unwrap() <= groups["groups"].as_u64().unwrap());
    assert!(groups["groups"].as_u64().unwrap() <= groups["requests"].as_u64().unwrap());
    (status, leases["result"].clone())
}

#[tokio::test]
async fn all_tools_match_routing_errors_and_domain_failures_do_not_partially_commit() {
    let mut r = Rig::new().await;
    for (i, name) in [(0, "alice"), (1, "bob")] {
        r.pair(
            Some(i),
            "agentchat_join",
            json!({"name":name}),
            &["join", "--name", name],
        )
        .await;
    }
    let post_key = Uuid::new_v4();
    let post = r
        .pair_id(
            Some(0),
            "agentchat_post",
            json!({"body":"@bob original"}),
            &["post", "--body", "@bob original"],
            post_key,
            None,
        )
        .await;
    let root = post["result"]["message_id"].as_str().unwrap().to_owned();
    let reply = r
        .pair(
            Some(1),
            "agentchat_reply",
            json!({"root_message_id":root,"body":"reply"}),
            &["reply", &root, "--body", "reply"],
        )
        .await;
    let reply_id = reply["result"]["message_id"].as_str().unwrap().to_owned();
    let inbox = r
        .pair(Some(1), "agentchat_inbox", json!({}), &["inbox"])
        .await;
    let row = inbox["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["dispatch"]["id"] == root)
        .expect("the posted Message must appear in its recipient's inbox");
    assert_eq!(row["dispatch"]["kind"], "message");
    assert_eq!(row["delivery"]["dispatch_id"], root);
    assert_eq!(row["message"]["id"], root);
    assert_eq!(row.get("notice"), Some(&Value::Null));
    let lease = r
        .pair(
            Some(0),
            "agentchat_reserve",
            json!({"patterns":["src/**"]}),
            &["reserve", "src/**"],
        )
        .await;
    let lease_id = lease["result"]["reservation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let generation = lease["result"]["generation"].as_i64().unwrap();
    let gen_arg = generation.to_string();
    let foreign = r.f.root.path().join("uninitialized");
    std::fs::create_dir(&foreign).unwrap();
    let destination = r.f.root.path().join("must-not-be-created.sqlite3");
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        ("agentchat_init", json!({}), vec!["install".into()]),
        ("agentchat_doctor", json!({}), vec!["doctor".into()]),
        ("agentchat_join", json!({}), vec!["join".into()]),
        ("agentchat_leave", json!({}), vec!["leave".into()]),
        ("agentchat_whoami", json!({}), vec!["whoami".into()]),
        (
            "agentchat_agents_list",
            json!({}),
            vec!["agents".into(), "list".into()],
        ),
        (
            "agentchat_channel_create",
            json!({"name":"route-check"}),
            vec!["channel".into(), "create".into(), "route-check".into()],
        ),
        (
            "agentchat_channel_join",
            json!({"name":"route-check"}),
            vec!["channel".into(), "join".into(), "route-check".into()],
        ),
        (
            "agentchat_channel_leave",
            json!({"name":"route-check"}),
            vec!["channel".into(), "leave".into(), "route-check".into()],
        ),
        (
            "agentchat_channel_list",
            json!({}),
            vec!["channel".into(), "list".into()],
        ),
        (
            "agentchat_channel_members",
            json!({"name":"dev"}),
            vec!["channel".into(), "members".into(), "dev".into()],
        ),
        (
            "agentchat_post",
            json!({"body":"never commit"}),
            vec!["post".into(), "--body".into(), "never commit".into()],
        ),
        (
            "agentchat_reply",
            json!({"root_message_id":root,"body":"never commit"}),
            vec![
                "reply".into(),
                root.clone(),
                "--body".into(),
                "never commit".into(),
            ],
        ),
        (
            "agentchat_message",
            json!({"message_id":root}),
            vec!["message".into(), root.clone()],
        ),
        ("agentchat_history", json!({}), vec!["history".into()]),
        (
            "agentchat_search",
            json!({"query":"x"}),
            vec!["search".into(), "x".into()],
        ),
        ("agentchat_inbox", json!({}), vec!["inbox".into()]),
        (
            "agentchat_read",
            json!({"message_ids":[root]}),
            vec!["read".into(), root.clone()],
        ),
        ("agentchat_events", json!({}), vec!["events".into()]),
        (
            "agentchat_reserve",
            json!({"patterns":["unused/**"]}),
            vec!["reserve".into(), "unused/**".into()],
        ),
        (
            "agentchat_reservations_list",
            json!({}),
            vec!["reservations".into(), "list".into()],
        ),
        (
            "agentchat_reservations_renew",
            json!({"reservation_id":lease_id,"generation":generation}),
            vec![
                "reservations".into(),
                "renew".into(),
                lease_id.clone(),
                "--generation".into(),
                gen_arg.clone(),
            ],
        ),
        (
            "agentchat_reservations_release",
            json!({"reservation_id":lease_id,"generation":generation}),
            vec![
                "reservations".into(),
                "release".into(),
                lease_id.clone(),
                "--generation".into(),
                gen_arg.clone(),
            ],
        ),
    ];
    let before = state(&mut r).await;
    for (tool, mut args, mut cli) in cases {
        args["project_path"] = json!(foreign);
        cli.extend(["--project".into(), foreign.to_str().unwrap().into()]);
        let refs = cli.iter().map(String::as_str).collect::<Vec<_>>();
        r.error(
            0,
            tool,
            args,
            &refs,
            if tool == "agentchat_init" {
                E::ProjectBindingMismatch
            } else {
                E::NotInitialized
            },
        )
        .await;
    }
    let names = r.names().await;
    assert_eq!(
        r.errors, names,
        "every tool must reach a shared routing error"
    );
    assert!(!foreign.join(".agentchat").exists());
    assert!(!destination.exists());
    assert_eq!(state(&mut r).await, before);
    r.error(
        0,
        "agentchat_init",
        json!({"new_project":true,"relocate":true}),
        &["install", "--new-project", "--relocate"],
        E::InvalidArgument,
    )
    .await;
    r.error(
        0,
        "agentchat_init",
        json!({"default_channels":["dev","","ops"]}),
        &["install", "--default-channels", "dev,,ops"],
        E::InvalidName,
    )
    .await;
    r.error(
        0,
        "agentchat_join",
        json!({"name":"changed"}),
        &["join", "--name", "changed"],
        E::IdentityConflict,
    )
    .await;
    r.error(
        0,
        "agentchat_channel_create",
        json!({"name":"bad/name"}),
        &["channel", "create", "bad/name"],
        E::InvalidName,
    )
    .await;
    r.error(
        0,
        "agentchat_channel_join",
        json!({"name":"missing"}),
        &["channel", "join", "missing"],
        E::NotFound,
    )
    .await;
    r.error(
        0,
        "agentchat_channel_leave",
        json!({"name":"dev"}),
        &["channel", "leave", "dev"],
        E::InvalidArgument,
    )
    .await;
    r.error(
        0,
        "agentchat_channel_members",
        json!({"name":"missing"}),
        &["channel", "members", "missing"],
        E::NotFound,
    )
    .await;
    r.error(
        0,
        "agentchat_post",
        json!({"body":" \t\n"}),
        &["post", "--body", " \t\n"],
        E::InvalidArgument,
    )
    .await;
    r.error(
        0,
        "agentchat_post",
        json!({"body":"different","request_id":post_key}),
        &["post", "--body", "different"],
        E::IdempotencyConflict,
    )
    .await;
    r.error(
        0,
        "agentchat_reply",
        json!({"root_message_id":reply_id,"body":"nested"}),
        &["reply", &reply_id, "--body", "nested"],
        E::InvalidThread,
    )
    .await;
    let unknown = Uuid::new_v4().to_string();
    r.error(
        0,
        "agentchat_message",
        json!({"message_id":unknown}),
        &["message", &unknown],
        E::NotFound,
    )
    .await;
    r.error(
        0,
        "agentchat_history",
        json!({"thread":reply_id}),
        &["history", "--thread", &reply_id],
        E::InvalidThread,
    )
    .await;
    let query = "界".repeat(342);
    r.error(
        0,
        "agentchat_search",
        json!({"query":query}),
        &["search", &query],
        E::InvalidArgument,
    )
    .await;
    r.error(
        1,
        "agentchat_read",
        json!({"message_ids":[root,unknown]}),
        &["read", &root, &unknown],
        E::NotFound,
    )
    .await;
    r.error(
        1,
        "agentchat_reserve",
        json!({"patterns":["src/**"]}),
        &["reserve", "src/**"],
        E::ReservationConflict,
    )
    .await;
    r.error(
        0,
        "agentchat_reserve",
        json!({"patterns":["valid/**","../outside"]}),
        &["reserve", "valid/**", "../outside"],
        E::InvalidArgument,
    )
    .await;
    let too_many: Vec<_> = (0..33).map(|i| format!("future/{i}.rs")).collect();
    let mut cli = vec!["reserve"];
    cli.extend(too_many.iter().map(String::as_str));
    r.error(
        0,
        "agentchat_reserve",
        json!({"patterns":too_many}),
        &cli,
        E::PatternTooComplex,
    )
    .await;
    r.error(
        1,
        "agentchat_reservations_renew",
        json!({"reservation_id":lease_id,"generation":generation}),
        &["reservations", "renew", &lease_id, "--generation", &gen_arg],
        E::StaleLease,
    )
    .await;
    let stale = generation + 1;
    let stale_arg = stale.to_string();
    r.error(
        0,
        "agentchat_reservations_release",
        json!({"reservation_id":lease_id,"generation":stale}),
        &[
            "reservations",
            "release",
            &lease_id,
            "--generation",
            &stale_arg,
        ],
        E::StaleLease,
    )
    .await;
    assert_eq!(state(&mut r).await, before);
    r.pair(Some(0), "agentchat_leave", json!({}), &["leave"])
        .await;
    // Leave commits business state before housekeeping retires its retained
    // attachment. Observe that lifecycle boundary before comparing snapshots;
    // a fixed application clock does not freeze the runtime's timer.
    let mut observer =
        agentchat_protocol::rpc::Client::connect(&r.runtime.paths().socket, r.project_id)
            .await
            .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let binding = observer.native_binding(r.contexts[0]).await.unwrap();
            let status = observer.health().await.unwrap();
            assert_eq!(
                binding.participation,
                agentchat_core::read::Participation::Left
            );
            let agent = binding.agent.unwrap();
            assert!(!agent.agent.joined);
            assert!(status.runtime.last_control_error.is_none());
            if !agent.connected
                && agent.presence == agentchat_core::model::Phase::Disconnected
                && status.runtime.activity.attached_connections == 1
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("left participant must reach observed terminal detachment");
    drop(observer);
    let left = state(&mut r).await;
    r.error(
        0,
        "agentchat_post",
        json!({"body":"after leave"}),
        &["post", "--body", "after leave"],
        E::NotJoined,
    )
    .await;
    assert_eq!(state(&mut r).await, left);
    r.close().await;
}

#[tokio::test]
async fn syntax_and_transport_rejections_keep_documented_error_shapes_and_utf8_limits() {
    let mut r = Rig::new().await;
    r.pair(
        Some(0),
        "agentchat_join",
        json!({"name":"alice"}),
        &["join", "--name", "alice"],
    )
    .await;
    let before = state(&mut r).await;
    for limit in [0, 201] {
        let arg = limit.to_string();
        let (mcp, code, cli) = r
            .outputs(
                Some(0),
                "agentchat_history",
                json!({"limit":limit}),
                &["history", "--limit", &arg],
                Uuid::new_v4(),
                None,
            )
            .await;
        assert_eq!(code, 2);
        assert_eq!(mcp["error"]["code"], cli["error"]["code"]);
        // CLI can reject before routing; the bound MCP already knows its project.
        assert!(cli["project_id"].is_null());
        assert_eq!(mcp["project_id"], json!(r.project_id));
    }
    let body = "界".repeat(21846);
    assert!(body.len() > 65536 && body.chars().count() < 65536);
    let (mcp, code, cli) = r
        .outputs(
            Some(0),
            "agentchat_post",
            json!({"body":body}),
            &["post", "--stdin"],
            Uuid::new_v4(),
            Some(body.as_bytes().to_vec()),
        )
        .await;
    assert_eq!(code, 2);
    assert_eq!(mcp["error"]["code"], "invalid_argument");
    assert_eq!(cli["error"]["code"], "invalid_argument");
    for args in [
        json!({"message_id":"not-a-uuid"}),
        json!({"message_id":Uuid::new_v4(),"arbitrary":true}),
    ] {
        let response = r.peers[0]
            .request(
                "tools/call",
                json!({"name":"agentchat_message","arguments":args}),
            )
            .await;
        // The pinned SDK reports DTO decoding as a tool error, without an
        // application envelope; it does not emit a JSON-RPC invalid-params error.
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(response["result"].get("structuredContent").is_none());
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("failed to deserialize parameters:")
        );
    }
    let cli = r.f.raw(&["--json", "message", "not-a-uuid"], None).await;
    assert_eq!(cli.status.code(), Some(2));
    let cli: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(cli["error"]["code"], "invalid_argument");
    assert_eq!(state(&mut r).await, before);
    r.close().await;
}
