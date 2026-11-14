//! Child exits at actual destructive cuts; OS ownership releases without Drop.
mod support;
use agentchat_core::{Result, model::HostKind};
use agentchat_hosts::{install::Installer, lifecycle::Lifecycle, setup::Environment};
use agentchat_runtime::{
    project::ProjectManager,
    removal::{RemovalProgress, RemovalStage},
};
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use support::Fixture;
use uuid::Uuid;

struct Crash {
    cut: String,
}
impl RemovalProgress for Crash {
    fn reached(&self, stage: RemovalStage) -> Result<()> {
        let matched = match self.cut.as_str() {
            "reference" => {
                stage
                    == RemovalStage::NodeRemoved {
                        component: 0,
                        node: 0,
                    }
            }
            "namespace" => {
                stage
                    == RemovalStage::NodeRemoved {
                        component: 1,
                        node: 0,
                    }
            }
            "configuration" => stage == RemovalStage::IntegrationRemoved { index: 0 },
            "global-root" => {
                stage
                    == RemovalStage::NodeRemoved {
                        component: 0,
                        node: 0,
                    }
            }
            _ => panic!("unknown fixture cut"),
        };
        if matched {
            std::process::exit(91);
        }
        Ok(())
    }
}

#[tokio::test]
#[ignore = "only the isolated parent supplies mandatory disposable roots and cut"]
async fn removal_crash_child() {
    let cwd =
        std::env::var_os("AGENTCHAT_REMOVAL_CRASH_WORKSPACE").expect("isolated child workspace");
    let cut = std::env::var("AGENTCHAT_REMOVAL_CRASH_CUT").unwrap();
    let id = Uuid::parse_str(&std::env::var("AGENTCHAT_REMOVAL_CRASH_REQUEST").unwrap()).unwrap();
    let global = matches!(cut.as_str(), "configuration" | "global-root");
    let manager = ProjectManager::from_environment()
        .unwrap()
        .with_removal_progress(Arc::new(Crash { cut }));
    // Both overrides are mandatory; a naked ignored-child invocation cannot
    // accidentally exercise the developer's selected installation/configuration.
    assert!(std::env::var_os("AGENTCHAT_HOME").is_some());
    assert!(std::env::var_os("AGENTCHAT_USER_CONFIG_ROOT").is_some());
    if global {
        manager.uninstall_global(id).await.unwrap();
    } else {
        manager
            .uninstall_project(std::path::PathBuf::from(cwd), id)
            .await
            .unwrap();
    }
    panic!("requested destructive cut was not reached");
}

#[tokio::test]
async fn abrupt_process_loss_recovers_config_reference_namespace_and_whole_root_cuts() {
    for cut in ["configuration", "reference", "namespace", "global-root"] {
        let f = Fixture::new();
        let original = f.request();
        let project = f
            .manager
            .initialize(&f.workspace, original.clone())
            .await
            .unwrap()
            .project;
        let environment = Environment {
            user_config_root: Some(f.config.clone()),
            ..Default::default()
        };
        let roots = environment.resolve().unwrap();
        fs::create_dir_all(roots.codex_config.parent().unwrap()).unwrap();
        fs::write(
            &roots.codex_config,
            "approval_policy = 'on-request'\n[mcp_servers.foreign]\ncommand = 'foreign'\n",
        )
        .unwrap();
        let installer = Installer::new(
            f.manager.home(),
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )
        .unwrap();
        installer
            .install(
                roots
                    .tool_artifacts(
                        &[HostKind::Codex],
                        &f.workspace.join("fixture-executable"),
                        f.manager.home(),
                    )
                    .unwrap(),
            )
            .await
            .unwrap();
        let id = Uuid::new_v4();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["removal_crash_child", "--exact", "--ignored", "--nocapture"])
            .env("AGENTCHAT_HOME", f.manager.home())
            .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
            .env("AGENTCHAT_REMOVAL_CRASH_WORKSPACE", &f.workspace)
            .env("AGENTCHAT_REMOVAL_CRASH_REQUEST", id.to_string())
            .env("AGENTCHAT_REMOVAL_CRASH_CUT", cut)
            .env_remove("AGENTCHAT_CONTEXT")
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(20), command.output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(91),
            "{cut}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let lifecycle = Lifecycle::new(f.manager.home()).unwrap();
        let journal = lifecycle.directory().join("pending-removal.json");
        assert!(journal.exists());
        assert_eq!(
            fs::metadata(&journal).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let error = f
            .manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap_err();
        assert_eq!(error.details["reason"], "removal_recovery_required");
        let global = matches!(cut, "configuration" | "global-root");
        let result = if global {
            f.manager
                .uninstall_global_with_environment(id, environment.clone())
                .await
                .unwrap()
        } else {
            f.manager
                .uninstall_project_with_environment(&f.workspace, id, environment.clone())
                .await
                .unwrap()
        };
        assert!(result.recovered);
        assert!(!journal.exists());
        assert!(!project.database_path().exists());
        assert_eq!(project.reference_path().exists(), global);
        assert!(
            fs::read_to_string(&roots.codex_config)
                .unwrap()
                .contains("approval_policy = 'on-request'")
        );
        assert!(f.manager.initialize(&f.workspace, original).await.is_err());
        if global {
            assert!(!f.manager.home().exists());
            assert!(
                std::path::Path::new(&result.backup_directory.unwrap().display)
                    .join("preimages.json")
                    .exists()
            );
            f.manager
                .uninstall_project_with_environment(&f.workspace, Uuid::new_v4(), environment)
                .await
                .unwrap();
        }
        let fresh = f
            .manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap()
            .project;
        assert_ne!(fresh.identity().id, project.identity().id);
    }
}
