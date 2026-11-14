//! Real executable user/observer operations; native selectors retain real proof requirements.

mod support;
use support::Fixture;
use uuid::Uuid;

#[tokio::test]
async fn uninitialized_and_invalid_inputs_create_no_project_state() {
    let f = Fixture::new();
    let (code, error) = f.run(&["history"]).await;
    assert_eq!(code, 3);
    assert_eq!(error["error"]["code"], "not_initialized");
    for (args, input) in [
        (vec!["--by-user", "post", "--stdin"], vec![b'x'; 65_537]),
        (vec!["--by-user", "post", "--stdin"], vec![0xff]),
        (
            vec!["--by-user", "reply", "bad-prefix", "--body", "hello"],
            vec![],
        ),
        (
            vec!["--by-user", "post", "--body", "literal", "--stdin"],
            vec![],
        ),
        (vec!["events", "--limit", "201"], vec![]),
        (vec!["status"], vec![]),
        (vec!["backup", "copy.sqlite3"], vec![]),
        (vec!["watch"], vec![]),
        (
            vec!["--by-user", "post", "--body", "x", "--read-timeout", "5m"],
            vec![],
        ),
        (
            vec!["install", "--delivery-confirmation-timeout", "1.5s"],
            vec![],
        ),
        (
            vec!["install", "--delivery-confirmation-timeout", "5w"],
            vec![],
        ),
        (
            vec!["install", "--delivery-confirmation-timeout", "4294967296s"],
            vec![],
        ),
        (vec!["whoami"], vec![]),
        (vec!["post", "--body", "mcp"], vec![]),
        (
            vec!["--by-user", "post", "--body", "mcp", "--unknown"],
            vec![],
        ),
        (vec!["--project", "mcp", "whoami"], vec![]),
    ] {
        assert_eq!(f.input(&args, Some(input)).await.0, 2, "{args:?}");
    }
    assert!(!f.manager.home().exists());
    assert!(!f.workspace.join(".agentchat").exists());
}

#[tokio::test]
async fn install_settings_retry_doctor_and_native_selector_refusal_report_actual_state() {
    let f = Fixture::new();
    let key = Uuid::new_v4().to_string();
    let args = [
        "install",
        "--request-id",
        &key,
        "--auto-read",
        "false",
        "--delivery-confirmation-timeout",
        "5m",
    ];
    let (code, first) = f.run(&args).await;
    assert_eq!(code, 0, "{first}");
    assert_eq!(f.run(&args).await.1, first);
    assert_eq!(
        first["result"]["receipt_settings"],
        serde_json::json!({"auto_read":false,"delivery_confirmation_timeout_seconds":300})
    );
    let project = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    let before = f.run(&["--by-user", "whoami"]).await.1;
    assert_eq!(before["result"]["participation"], "unjoined");
    assert!(before["result"]["actor"].is_null());
    let context = f.context("unadmitted").await;
    let inherited = f
        .command(&["--json", "whoami"])
        .env("AGENTCHAT_CONTEXT", context.context_id.to_string())
        .output()
        .await
        .unwrap();
    assert_eq!(inherited.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&inherited.stdout).unwrap()["error"]["code"],
        "invalid_argument"
    );
    let (code, error) = f
        .run(&[
            "--by-host",
            "codex",
            "--by-agent-id",
            &context.host_session_id,
            "whoami",
        ])
        .await;
    assert_eq!(code, 4, "{error}");
    assert_eq!(error["error"]["code"], "session_required");
    let after = f.run(&["--by-user", "whoami"]).await.1;
    assert_eq!(before["result"]["head"], after["result"]["head"]);
    assert_eq!(
        before["result"]["statistics"],
        after["result"]["statistics"]
    );
    let (code, doctor) = f.run(&["doctor"]).await;
    assert_eq!(code, 0, "{doctor}");
    assert_eq!(doctor["result"]["database"]["integrity"], "ok");
    f.stop_lazy(&project).await;
}

#[tokio::test]
async fn separate_cli_processes_share_user_identity_scopes_receipts_and_leases() {
    let f = Fixture::new();
    let project = f.initialize().await;
    let (handle, task) = f.server(&project).await;
    assert_eq!(
        f.run(&["--by-user", "history"]).await.1["error"]["code"],
        "not_joined"
    );
    let joined = f.run(&["--by-user", "join"]).await;
    assert_eq!(joined.0, 0, "{}", joined.1);
    assert_eq!(
        f.run(&["--by-user", "join"]).await.1["result"]["actor_id"],
        joined.1["result"]["actor_id"]
    );
    assert_eq!(
        f.run(&["--by-user", "channel", "create", "review"]).await.0,
        0
    );
    assert_eq!(
        f.run(&["channel", "members", "review"]).await.1["result"]["items"],
        serde_json::json!([])
    );
    assert_eq!(
        f.run(&["--by-user", "channel", "join", "review"]).await.0,
        0
    );
    let body = "literal $(touch SHOULD_NOT_EXIST) `echo no`\n日本語\0complete";
    let key = Uuid::new_v4().to_string();
    let args = [
        "--by-user",
        "post",
        "--stdin",
        "--channel",
        "review",
        "--request-id",
        &key,
    ];
    let (code, posted) = f.input(&args, Some(body.as_bytes().to_vec())).await;
    assert_eq!(code, 0, "{posted}");
    assert_eq!(
        f.input(&args, Some(body.as_bytes().to_vec())).await.1,
        posted
    );
    assert!(!f.workspace.join("SHOULD_NOT_EXIST").exists());
    let root = posted["result"]["message_id"].as_str().unwrap();
    let detail = f.run(&["message", root]).await.1;
    assert_eq!(detail["result"]["message"]["body"], body);
    assert_eq!(detail["result"]["message"]["author"]["kind"], "user");
    assert_eq!(f.run(&["--by-user", "read", root]).await.0, 0);
    let detail = f.run(&["message", root, "--limit", "1"]).await.1;
    let receipt = &detail["result"]["receipts"]["items"][0];
    assert_eq!(receipt["recipient"]["kind"], "user");
    assert!(receipt["delivery"].is_null());
    assert_eq!(receipt["reads"][0]["kind"], "explicit");
    assert_eq!(
        f.run(&["--by-user", "reply", root, "--body", "reply"])
            .await
            .0,
        0
    );
    assert_eq!(
        f.run(&["history", "--thread", root]).await.1["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        f.run(&["search", "$(touch", "--channel", "review"]).await.1["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for args in [vec!["--by-user", "inbox"]] {
        let result = f.run(&args).await.1;
        assert_eq!(result["error"]["details"]["reason"], "nonreceiving_actor");
    }
    let lease = f
        .run(&["--by-user", "reserve", "src/**", "--ttl", "30"])
        .await
        .1;
    assert_eq!(lease["result"]["owner"]["kind"], "user");
    let id = lease["result"]["reservation_id"].as_str().unwrap();
    let generation = lease["result"]["generation"].as_i64().unwrap().to_string();
    for op in ["renew", "release"] {
        assert_eq!(
            f.run(&[
                "--by-user",
                "reservations",
                op,
                id,
                "--generation",
                &generation
            ])
            .await
            .0,
            0
        );
    }
    let members = f.run(&["channel", "members", "review"]).await.1;
    assert_eq!(members["result"]["items"][0]["kind"], "user");
    assert_eq!(
        f.run(&["agents", "list"]).await.1["result"]["items"],
        serde_json::json!([])
    );
    assert_eq!(
        f.run(&["--by-user", "channel", "leave", "review"]).await.0,
        0
    );
    assert_eq!(
        f.run(&["--by-user", "history", "--channel", "review"])
            .await
            .1["error"]["code"],
        "not_joined"
    );
    assert_eq!(f.run(&["history", "--channel", "review"]).await.0, 0);
    assert_eq!(f.run(&["--by-user", "leave"]).await.0, 0);
    assert_eq!(
        f.run(&["--by-user", "history"]).await.1["error"]["code"],
        "not_joined"
    );
    assert_eq!(
        f.run(&["--by-user", "whoami"]).await.1["result"]["participation"],
        "left"
    );
    handle.shutdown();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn new_project_retry_recovers_its_result_with_explicit_project_routing() {
    let f = Fixture::new();
    let first = f.initialize().await;
    let key = Uuid::new_v4().to_string();
    let args = ["install", "--new-project", "--request-id", &key];
    let (code, created) = f.run(&args).await;
    assert_eq!(code, 0, "{created}");
    assert_ne!(created["project_id"], first.identity().id.to_string());
    assert_eq!(f.run(&args).await.1, created);
    assert_eq!(
        f.manager
            .discover(&f.workspace)
            .await
            .unwrap()
            .unwrap()
            .identity()
            .id
            .to_string(),
        created["project_id"]
    );
}

#[tokio::test]
async fn real_finite_output_routes_plain_success_and_failure_without_json_fallback() {
    let f = Fixture::new();
    let project = f.initialize().await;
    let (handle, task) = f.server(&project).await;
    assert_eq!(f.run(&["--by-user", "join"]).await.0, 0);
    let literal_flag = f
        .raw(&["--by-user", "post", "--body", "--json"], None)
        .await;
    assert!(literal_flag.status.success());
    assert!(literal_flag.stderr.is_empty());
    assert!(serde_json::from_slice::<serde_json::Value>(&literal_flag.stdout).is_err());
    let body = "first\n日本語\0\u{001b}[31m\nlast  ";
    let posted = f
        .input(&["--by-user", "post"], Some(body.as_bytes().to_vec()))
        .await
        .1;
    let id = posted["result"]["message_id"].as_str().unwrap();
    let shown = f.raw(&["message", id], None).await;
    assert!(shown.status.success() && shown.stderr.is_empty());
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(shown.contains("first\n日本語\\u{0000}\\u{001b}[31m\nlast"));
    assert!(!shown.contains('\u{001b}'));
    let failed = f.raw(&["--by-user", "inbox"], None).await;
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(
        String::from_utf8(failed.stderr)
            .unwrap()
            .contains("nonreceiving_actor")
    );
    let json = f.run(&["--by-user", "inbox"]).await;
    assert_eq!(json.1["error"]["details"]["reason"], "nonreceiving_actor");
    handle.shutdown();
    task.await.unwrap().unwrap();
}
