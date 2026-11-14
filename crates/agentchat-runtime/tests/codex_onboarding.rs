//! Tool-only init stays immutable and isolated launch cannot reach real settings.
mod support;

use agentchat_core::{ErrorCode, Result, model::HostKind, project::Scope};
use agentchat_hosts::{
    launch::NativeTerminal,
    setup::{Environment, ToolSetup},
};
use agentchat_protocol::operations::{Backend, Operation, Target};
use agentchat_runtime::{
    operations::{Launcher, NativeLaunch, Operations},
    project::Project,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc};
use uuid::Uuid;

struct NoLaunch;
#[async_trait::async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        panic!("tool-only initialization or launch refusal must not start a runtime")
    }
}

async fn init(operations: &Operations, workspace: &Path, id: Uuid) -> Value {
    serde_json::to_value(
        operations
            .invoke(
                id,
                Target {
                    project_path: Some(workspace.to_owned()),
                    ..Target::default()
                },
                Operation::Initialize {
                    auto_read: None,
                    delivery_confirmation_timeout_seconds: None,
                    default_channels: None,
                    scope: Some(Scope::Directory),
                    relocate: false,
                    new_project: false,
                },
            )
            .await,
    )
    .unwrap()
}

#[tokio::test]
async fn stopped_daemon_tool_only_init_has_frozen_retries_and_isolated_launch_refuses() {
    let f = support::Fixture::new();
    let bin = f.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let cli = bin.join("codex");
    fs::write(
        &cli,
        "#!/bin/sh\nif [ \"$*\" = 'app-server daemon version' ]; then\n\
         printf 'inspect\\n' >> \"$0.calls\"\nexit 21\nelse\n\
         printf 'unexpected\\n' >> \"$0.unexpected\"\nexit 93\nfi\n",
    )
    .unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    // Establish the new fixture's executable readiness before exercising the
    // product's unchanged five-second discovery boundary.
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::process::Command::new(&cli)
            .args(["app-server", "daemon", "version"])
            .kill_on_drop(true)
            .status(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(status.code(), Some(21));
    fs::remove_file(cli.with_extension("calls")).unwrap();
    let configuration = f.config.join(".codex/config.toml");
    fs::create_dir_all(configuration.parent().unwrap()).unwrap();
    let foreign = "approval_policy='on-request'\n[mcp_servers.foreign]\ncommand='unchanged'\n";
    fs::write(&configuration, foreign).unwrap();
    let setup = ToolSetup::new(
        f.manager.home().to_owned(),
        f.root.path().join("agentchat"),
        Environment {
            user_config_root: Some(f.config.clone()),
            ..Environment::default()
        },
        Some(bin.into_os_string()),
    )
    .with_native_discovery();
    let operations = Operations::new(f.manager.clone(), Arc::new(NoLaunch)).with_host_tools(setup);
    let request = Uuid::new_v4();
    let first = init(&operations, &f.workspace, request).await;
    assert!(first.get("error").is_none(), "{first}");
    assert_eq!(first["result"]["integrations"]["state"], "partial");
    assert_eq!(
        first["result"]["integrations"]["hosts"]["codex"]["state"],
        "tools_installed"
    );
    assert_eq!(
        first["result"]["integrations"]["hosts"]["codex"]["components"],
        json!(["mcp", "skill"])
    );
    assert_eq!(
        first["result"]["integrations"]["lifecycle"]["state"],
        "unavailable"
    );
    let installed = fs::read(&configuration).unwrap();
    assert!(installed.starts_with(foreign.as_bytes()));
    let receipt = f
        .manager
        .home()
        .join(format!("integrations/requests/{request}.json"));
    let original_receipt = fs::read(&receipt).unwrap();
    let route = operations
        .route(&Target {
            project_path: Some(f.workspace.clone()),
            ..Target::default()
        })
        .await
        .unwrap();
    let error = operations
        .launch_native(
            &route,
            NativeLaunch {
                host: HostKind::Codex,
                resume: None,
                name: Some("alpha".into()),
                terminal: NativeTerminal::Inherited,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigurationConflict);
    let calls = cli.with_extension("calls");
    assert_eq!(fs::read_to_string(&calls).unwrap(), "inspect\n");
    assert_eq!(init(&operations, &f.workspace, request).await, first);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "inspect\n");
    assert_eq!(fs::read(&configuration).unwrap(), installed);
    assert_eq!(fs::read(&receipt).unwrap(), original_receipt);
    let fresh = init(&operations, &f.workspace, Uuid::new_v4()).await;
    assert!(fresh.get("error").is_none(), "{fresh}");
    assert_eq!(fresh["project_id"], first["project_id"]);
    assert_eq!(fresh["result"]["created"], false);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "inspect\ninspect\n");
    assert_eq!(fs::read(&receipt).unwrap(), original_receipt);
    assert!(!cli.with_extension("unexpected").exists());
    assert!(!f.root.path().join("absent.sock").exists());
}
