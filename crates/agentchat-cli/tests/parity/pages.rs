//! Real pipes preserve literal bodies, finite pages and independent read state.
use super::rig::Rig;
use agentchat_core::{application::Mutation, read::MAX_PAGE_BYTES};
use serde_json::json;
use std::collections::BTreeSet;
use uuid::Uuid;

#[tokio::test]
async fn paging_filters_complete_utf8_bodies_and_byte_budgets_match_without_implicit_reads() {
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
    r.seed(
        0,
        (0..51).map(|i| Mutation::Post {
            channel: Some("dev".into()),
            body: format!("needle page {i}"),
        }),
    )
    .await;
    let mut body = "@bob ".to_owned();
    body.push_str(&"🙂\"\\\n".repeat((65536 - 5) / 7));
    body.push_str(&"x".repeat(65536 - body.len()));
    assert_eq!(body.len(), 65536);
    let post = r
        .pair_id(
            Some(0),
            "agentchat_post",
            json!({"body":body}),
            &["post", "--stdin"],
            Uuid::new_v4(),
            Some(body.as_bytes().to_vec()),
        )
        .await;
    let root = post["result"]["message_id"].as_str().unwrap().to_owned();
    let page = r
        .pair(Some(0), "agentchat_history", json!({}), &["history"])
        .await;
    assert_eq!(page["result"]["items"].as_array().unwrap().len(), 50);
    let after = page["result"]["next_cursor"].as_str().unwrap().to_owned();
    let tail = r
        .pair(
            Some(0),
            "agentchat_history",
            json!({"after":after}),
            &["history", "--after", &after],
        )
        .await;
    assert_eq!(tail["result"]["items"].as_array().unwrap().len(), 2);
    assert!(tail["result"]["next_cursor"].is_null());
    let detail = r
        .pair(
            Some(1),
            "agentchat_message",
            json!({"message_id":root}),
            &["message", &root],
        )
        .await;
    assert_eq!(detail["result"]["message"]["body"], body);
    let before_big = post["result"]["seq"].as_str().unwrap().to_owned();
    let big = r
        .seed(
            0,
            (0..13).map(|_| Mutation::Post {
                channel: Some("dev".into()),
                body: body.clone(),
            }),
        )
        .await;
    let expected = big
        .iter()
        .map(|p| p["message_id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut after = before_big;
    loop {
        let page = r
            .pair(
                Some(1),
                "agentchat_history",
                json!({"after":after,"limit":200}),
                &["history", "--after", &after, "--limit", "200"],
            )
            .await;
        assert!(serde_json::to_vec(&page["result"]).unwrap().len() <= MAX_PAGE_BYTES);
        let rows = page["result"]["items"].as_array().unwrap();
        assert!(!rows.is_empty() && rows.len() < 13);
        for row in rows {
            assert_eq!(row["body"], body);
            assert!(seen.insert(row["id"].as_str().unwrap().to_owned()));
        }
        if page["result"]["next_cursor"].is_null() {
            break;
        }
        after = page["result"]["next_cursor"].as_str().unwrap().to_owned();
    }
    assert_eq!(seen, expected);
    r.pair(
        Some(0),
        "agentchat_channel_create",
        json!({"name":"review"}),
        &["channel", "create", "review"],
    )
    .await;
    for i in 0..2 {
        r.pair(
            Some(i),
            "agentchat_channel_join",
            json!({"name":"review"}),
            &["channel", "join", "review"],
        )
        .await;
    }
    let filtered = r
        .pair(
            Some(0),
            "agentchat_post",
            json!({"channel":"review","body":"needle %_"}),
            &["post", "--channel", "review", "--body", "needle %_"],
        )
        .await;
    let thread = filtered["result"]["message_id"]
        .as_str()
        .unwrap()
        .to_owned();
    r.pair(
        Some(1),
        "agentchat_reply",
        json!({"root_message_id":thread,"body":"thread reply"}),
        &["reply", &thread, "--body", "thread reply"],
    )
    .await;
    let thread_page = r
        .pair(
            Some(0),
            "agentchat_history",
            json!({"channel":"review","thread":thread}),
            &["history", "--channel", "review", "--thread", &thread],
        )
        .await;
    assert_eq!(thread_page["result"]["items"].as_array().unwrap().len(), 2);
    let literal = r
        .pair(
            Some(0),
            "agentchat_search",
            json!({"query":"%_","channel":"review"}),
            &["search", "%_", "--channel", "review"],
        )
        .await;
    assert_eq!(literal["result"]["items"].as_array().unwrap().len(), 1);
    for (tool, cli) in [
        ("agentchat_agents_list", vec!["agents", "list"]),
        ("agentchat_channel_list", vec!["channel", "list"]),
    ] {
        let mut first = cli.clone();
        first.extend(["--limit", "1"]);
        let p = r.pair(Some(0), tool, json!({"limit":1}), &first).await;
        assert_eq!(p["result"]["items"].as_array().unwrap().len(), 1);
        let after = p["result"]["next_cursor"].as_str().unwrap().to_owned();
        let mut last = cli.clone();
        last.extend(["--limit", "1", "--after", &after]);
        let p = r
            .pair(Some(0), tool, json!({"limit":1,"after":after}), &last)
            .await;
        assert_eq!(p["result"]["items"].as_array().unwrap().len(), 1);
    }
    r.pair(
        Some(0),
        "agentchat_channel_members",
        json!({"name":"review","limit":1}),
        &["channel", "members", "review", "--limit", "1"],
    )
    .await;
    r.pair(
        Some(0),
        "agentchat_events",
        json!({"channel":"review","after":"0","limit":200}),
        &[
            "events",
            "--channel",
            "review",
            "--after",
            "0",
            "--limit",
            "200",
        ],
    )
    .await;
    r.pair(
        Some(1),
        "agentchat_inbox",
        json!({"limit":1}),
        &["inbox", "--limit", "1"],
    )
    .await;
    let status = r
        .pair(Some(0), "agentchat_whoami", json!({}), &["whoami"])
        .await;
    assert_eq!(status["result"]["statistics"]["read_receipts"], 0);
    assert!(
        status["result"]["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    for actor in [0, 1] {
        r.pair(
            Some(actor),
            "agentchat_read",
            json!({"message_ids":[root]}),
            &["read", &root],
        )
        .await;
    }
    let readers = r
        .pair(
            Some(0),
            "agentchat_message",
            json!({"message_id":root,"limit":1}),
            &["message", &root, "--limit", "1"],
        )
        .await;
    let after = readers["result"]["receipts"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let readers = r
        .pair(
            Some(0),
            "agentchat_message",
            json!({"message_id":root,"limit":1,"after":after}),
            &["message", &root, "--limit", "1", "--after", &after],
        )
        .await;
    assert_eq!(
        readers["result"]["receipts"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(readers["result"]["receipts"]["next_cursor"].is_null());
    r.close().await;
}
