//! A real child exits without destructors while its durable session is live.

mod support;
use agentchat_core::{application::Application, read::Query, time::Clock};
use agentchat_runtime::{context::ContextRegistry, daemon::Server, project::ProjectManager};
use std::{process::Command, sync::Arc};
use support::{
    Fixture, SyntheticHost,
    runtime::{ManualClock, attach, join, mutate, options, query},
};

#[tokio::test]
async fn abrupt_exit_liveness_is_invalidated_before_startup_lease_renewal() {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "runtime_process_child"])
        .env("AGENTCHAT_HOME", f.manager.home())
        .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
        .env("AGENTCHAT_RUNTIME_TEST_WORKSPACE", &f.workspace)
        .status()
        .unwrap();
    assert_eq!(result.code(), Some(79));
    let log = project.data_dir().join("logs/runtime.jsonl");
    let before = std::fs::read(&log).unwrap();
    let complete = &before[..before.iter().rposition(|b| *b == b'\n').unwrap() + 1];
    let records = String::from_utf8_lossy(complete)
        .lines()
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[0]["event"]["kind"], "started");
    assert!(records.iter().all(|row| row["event"]["kind"] != "stopped"));
    let clock = Arc::new(ManualClock::default());
    clock.advance(10);
    let app = Application::new(
        f.manager.open(&project).await.unwrap(),
        clock.clone(),
        Arc::new(SyntheticHost),
    );
    let retained = app
        .query(
            None,
            Query::Agents {
                page: Default::default(),
            },
        )
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert_eq!(
        retained["items"][0]["connected"], true,
        "the child must leave actual persisted live state"
    );
    drop(app);
    let server = Server::bind(
        f.manager.clone(),
        project.clone(),
        Arc::new(SyntheticHost),
        clock.clone(),
        options(),
    )
    .await
    .unwrap();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let mut client =
        agentchat_protocol::rpc::Client::connect(&handle.paths().socket, project.identity().id)
            .await
            .unwrap();
    let agents = query(
        &mut client,
        None,
        Query::Agents {
            page: Default::default(),
        },
    )
    .await;
    assert_eq!(agents["items"][0]["connected"], false);
    let groups = query(
        &mut client,
        None,
        Query::Reservations {
            owner: None,
            all: true,
            page: Default::default(),
        },
    )
    .await;
    assert_eq!(
        groups["items"][0]["expires_at"],
        clock.now().after_seconds(20).unwrap().to_string()
    );
    drop(client);
    handle.shutdown();
    task.await.unwrap().unwrap();
    assert!(!handle.paths().socket.exists());
    let after = std::fs::read(&log).unwrap();
    assert!(after.starts_with(complete));
    let records = String::from_utf8_lossy(&after)
        .lines()
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.last().unwrap()["event"]["kind"], "stopped");
    assert_eq!(
        records.last().unwrap()["instance_id"],
        handle.instance_id().to_string()
    );
}

#[tokio::test]
#[ignore = "subprocess helper is explicitly invoked by the crash parent"]
async fn runtime_process_child() {
    let workspace =
        std::path::PathBuf::from(std::env::var_os("AGENTCHAT_RUNTIME_TEST_WORKSPACE").unwrap());
    let manager = ProjectManager::from_environment().unwrap();
    let project = manager.discover(&workspace).await.unwrap().unwrap();
    let clock = Arc::new(ManualClock::default());
    let context = ContextRegistry::new(manager.clone())
        .issue(agentchat_runtime::context::ContextSeed {
            host: agentchat_core::model::HostKind::Codex,
            host_session_id: "abrupt-exact-root".into(),
            host_version: "fixture-only".into(),
            os_profile: "synthetic-not-admitted".into(),
            working_directory: workspace,
        })
        .await
        .unwrap();
    let server = Server::bind(
        manager,
        project.clone(),
        Arc::new(SyntheticHost),
        clock,
        options(),
    )
    .await
    .unwrap();
    let handle = server.handle();
    let _task = tokio::spawn(server.run());
    let mut client =
        agentchat_protocol::rpc::Client::connect(&handle.paths().socket, project.identity().id)
            .await
            .unwrap();
    join(&mut client, &context, "alice").await;
    attach(&mut client, &context).await;
    mutate(
        &mut client,
        &context,
        agentchat_core::application::Mutation::Reserve {
            patterns: vec!["src/**".into()],
            ttl_seconds: Some(30),
            reason: None,
        },
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !std::fs::read(project.data_dir().join("logs/runtime.jsonl"))
            .unwrap()
            .contains(&b'\n')
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // No Rust drops, connection cleanup, final detach or OS lock release helper.
    std::process::exit(79);
}
