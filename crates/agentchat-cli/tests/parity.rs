//! MCP and typed CLI adaptation share operations over a real project/runtime.
//! Native contexts here are test-owned private fixtures, not public selector proof.
#[allow(dead_code)]
#[path = "../src/adapt.rs"]
mod adapt;
#[allow(dead_code, unused_imports)]
#[path = "../src/args.rs"]
mod args;
#[allow(dead_code)]
#[path = "../src/duration.rs"]
mod duration;
#[path = "parity/errors.rs"]
mod errors;
#[allow(dead_code)]
#[path = "../src/input.rs"]
mod input;
#[path = "parity/pages.rs"]
mod pages;
#[path = "parity/rig.rs"]
mod rig;
mod support;
#[allow(dead_code)]
#[path = "../src/terminal.rs"]
mod terminal;
use rig::Rig;
use serde_json::json;

#[tokio::test]
async fn every_advertised_tool_matches_cli_results_defaults_and_successful_retries() {
    let mut r = Rig::new().await;
    r.pair(
        None,
        "agentchat_init",
        json!({"default_channels":[" Ops ","general","ops"]}),
        &["install", "--default-channels", " Ops ,general,ops"],
    )
    .await;
    r.pair(None, "agentchat_doctor", json!({}), &["doctor"])
        .await;
    for (i, name) in [(0, "alice"), (1, "bob")] {
        r.pair(
            Some(i),
            "agentchat_join",
            json!({"name":name}),
            &["join", "--name", name],
        )
        .await;
    }
    r.pair(Some(0), "agentchat_whoami", json!({}), &["whoami"])
        .await;
    r.pair(
        Some(0),
        "agentchat_agents_list",
        json!({}),
        &["agents", "list"],
    )
    .await;
    r.pair(
        Some(0),
        "agentchat_channel_create",
        json!({"name":"review"}),
        &["channel", "create", "review"],
    )
    .await;
    let empty = r
        .pair(
            Some(0),
            "agentchat_channel_members",
            json!({"name":"review"}),
            &["channel", "members", "review"],
        )
        .await;
    assert!(empty["result"]["items"].as_array().unwrap().is_empty());
    for i in 0..2 {
        r.pair(
            Some(i),
            "agentchat_channel_join",
            json!({"name":"review"}),
            &["channel", "join", "review"],
        )
        .await;
    }
    r.pair(
        Some(0),
        "agentchat_channel_list",
        json!({}),
        &["channel", "list"],
    )
    .await;
    r.pair(
        Some(0),
        "agentchat_channel_list",
        json!({"state":"all"}),
        &["channel", "list", "--state", "all"],
    )
    .await;
    let members = r
        .pair(
            Some(0),
            "agentchat_channel_members",
            json!({"name":"review"}),
            &["channel", "members", "review"],
        )
        .await;
    assert_eq!(members["result"]["items"].as_array().unwrap().len(), 2);
    let body =
        "--version @bob @missing $(touch PARITY_SHOULD_NOT_EXIST) `echo no`\n全文\t\"literal\"";
    let posted = r
        .pair(
            Some(0),
            "agentchat_post",
            json!({"body":body}),
            &["post", "--body", body],
        )
        .await;
    assert_eq!(posted["result"]["warnings"][0]["code"], "unknown_recipient");
    let root = posted["result"]["message_id"].as_str().unwrap().to_owned();
    let message = r
        .pair(
            Some(0),
            "agentchat_message",
            json!({"message_id":root}),
            &["message", &root],
        )
        .await;
    assert_eq!(message["result"]["message"]["body"], body);
    let reply = r
        .pair(
            Some(1),
            "agentchat_reply",
            json!({"root_message_id":root,"body":"literal reply"}),
            &["reply", &root, "--body", "literal reply"],
        )
        .await;
    let reply_id = reply["result"]["message_id"].as_str().unwrap().to_owned();
    let history = r
        .pair(Some(0), "agentchat_history", json!({}), &["history"])
        .await;
    assert_eq!(history["result"]["items"].as_array().unwrap().len(), 2);
    r.pair(
        Some(0),
        "agentchat_search",
        json!({"query":"$(touch"}),
        &["search", "$(touch"],
    )
    .await;
    r.pair(Some(0), "agentchat_events", json!({}), &["events"])
        .await;
    r.pair(Some(1), "agentchat_inbox", json!({}), &["inbox"])
        .await;
    let unread = r
        .pair(
            Some(0),
            "agentchat_message",
            json!({"message_id":root}),
            &["message", &root],
        )
        .await;
    assert!(
        unread["result"]["receipts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["reads"].as_array().unwrap().is_empty())
    );
    r.pair(
        Some(1),
        "agentchat_read",
        json!({"message_ids":[root,reply_id]}),
        &["read", &root, &reply_id],
    )
    .await;
    let read = r
        .pair(
            Some(0),
            "agentchat_message",
            json!({"message_id":root}),
            &["message", &root],
        )
        .await;
    assert_eq!(
        read["result"]["receipts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| !row["reads"].as_array().unwrap().is_empty())
            .count(),
        1
    );
    let combined = r
        .pair(
            Some(0),
            "agentchat_reply",
            json!({"root_message_id":root,"body":"@bob read and accepted","read_message_ids":[reply_id,reply_id]}),
            &["reply", &root, "--body", "@bob read and accepted", "--read-message-id", &reply_id, "--read-message-id", &reply_id],
        )
        .await;
    assert_eq!(
        combined["result"]["read_result"]["changed"],
        json!([reply_id])
    );
    let lease = r
        .pair(
            Some(0),
            "agentchat_reserve",
            json!({"patterns":["src/**/*.rs","docs/**"]}),
            &["reserve", "src/**/*.rs", "docs/**"],
        )
        .await;
    let lease_id = lease["result"]["reservation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let generation = lease["result"]["generation"].as_i64().unwrap();
    let gen_arg = generation.to_string();
    r.pair(
        Some(0),
        "agentchat_reservations_list",
        json!({}),
        &["reservations", "list"],
    )
    .await;
    r.pair(
        Some(0),
        "agentchat_reservations_renew",
        json!({"reservation_id":lease_id,"generation":generation}),
        &["reservations", "renew", &lease_id, "--generation", &gen_arg],
    )
    .await;
    r.pair(
        Some(0),
        "agentchat_reservations_release",
        json!({"reservation_id":lease_id,"generation":generation}),
        &[
            "reservations",
            "release",
            &lease_id,
            "--generation",
            &gen_arg,
        ],
    )
    .await;
    r.pair(
        Some(0),
        "agentchat_channel_leave",
        json!({"name":"review"}),
        &["channel", "leave", "review"],
    )
    .await;
    r.pair(Some(0), "agentchat_leave", json!({}), &["leave"])
        .await;
    assert!(!r.f.workspace.join("PARITY_SHOULD_NOT_EXIST").exists());
    r.finish().await;
}
