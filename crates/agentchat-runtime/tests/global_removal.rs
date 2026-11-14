//! Whole-root cleanup uses temporary state/config roots and no native execution.
mod support;
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use agentchat_hosts::{
    claude::observer::NativeProfile,
    install::Installer,
    lifecycle::Lifecycle,
    setup::{CodexProfile, Environment, Roots},
};
use agentchat_protocol::removal::RemovalState;
use agentchat_runtime::removal::{RemovalProgress, RemovalStage};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use support::Fixture;
use uuid::Uuid;

fn environment(f: &Fixture, overridden: bool) -> Environment {
    Environment {
        home: Some(f.root.path().join("unused-home")),
        user_config_root: Some(f.config.clone()),
        claude_config_dir: overridden.then(|| f.config.join("custom-claude")),
        ..Default::default()
    }
}
async fn integrations(f: &Fixture, env: &Environment) -> Roots {
    let roots = env.resolve().unwrap();
    fs::create_dir_all(roots.codex_config.parent().unwrap()).unwrap();
    fs::create_dir_all(roots.claude_settings.parent().unwrap()).unwrap();
    if !roots.codex_config.exists() {
        fs::write(&roots.codex_config,"# preserved\napproval_policy = 'on-request'\n[mcp_servers.foreign]\ncommand = 'foreign'\n[mcp_servers.ac]\ncommand = 'legacy'\n").unwrap();
        fs::write(&roots.claude_registry,json!({"oauth":{"token":"private-fixture-secret"},"mcpServers":{"foreign":{"command":"foreign"}}}).to_string()).unwrap();
        fs::write(&roots.claude_settings,json!({"sandbox":{"enabled":true},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign"}]}]}}).to_string()).unwrap();
    }
    let native = f.workspace.join("native ' $literal");
    let executable = f.workspace.join("agentchat executable");
    fs::write(&native, b"native executable remains").unwrap();
    fs::write(&executable, b"agentchat executable remains").unwrap();
    let mut artifacts = roots
        .codex_native_artifacts(
            &executable,
            f.manager.home(),
            &CodexProfile {
                executable: native.clone(),
                version: "fixture-only".into(),
            },
        )
        .unwrap();
    artifacts.extend(
        roots
            .claude_native_artifacts(
                &executable,
                f.manager.home(),
                &NativeProfile {
                    executable: native,
                    version: "fixture-only".into(),
                },
            )
            .unwrap(),
    );
    Installer::new(
        f.manager.home(),
        roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
    )
    .unwrap()
    .install(artifacts)
    .await
    .unwrap();
    roots
}
fn json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
async fn history(
    manager: &agentchat_runtime::project::ProjectManager,
    project: &agentchat_runtime::project::Project,
    cwd: &std::path::Path,
    body: &str,
) {
    use agentchat_core::{
        application::{Application, Mutation},
        model::Context,
        time::SystemClock,
    };
    let store = manager.open(project).await.unwrap();
    let context = Context {
        context_id: Uuid::new_v4(),
        project_id: project.identity().id,
        incarnation: Uuid::new_v4(),
        host: HostKind::Codex,
        host_session_id: Uuid::new_v4().to_string(),
        host_version: "fixture-only".into(),
        os_profile: "synthetic-not-admitted".into(),
        working_directory: cwd.into(),
    };
    let app = Application::new(
        store.clone(),
        Arc::new(SystemClock),
        Arc::new(support::SyntheticHost),
    );
    app.execute(
        Uuid::new_v4(),
        &context,
        Mutation::Join {
            name: Some("history-owner".into()),
        },
    )
    .await
    .unwrap();
    app.execute(
        Uuid::new_v4(),
        &context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: body.into(),
        },
    )
    .await
    .unwrap();
    let status = serde_json::to_value(app.health().await.unwrap()).unwrap();
    assert_eq!(status["statistics"]["messages"], 1);
    drop(app);
    Arc::try_unwrap(store)
        .ok()
        .expect("exclusive store fixture")
        .close()
        .await
        .unwrap();
}
struct FailOnce {
    stage: RemovalStage,
    fired: AtomicBool,
}
impl RemovalProgress for FailOnce {
    fn reached(&self, stage: RemovalStage) -> Result<()> {
        if stage == self.stage && !self.fired.swap(true, Ordering::SeqCst) {
            return Err(Error::new(
                ErrorCode::IoError,
                "Injected global removal interruption.",
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn global_removes_multiple_namespaces_retains_references_and_supports_both_reinstall_routes()
{
    for overridden in [false, true] {
        let f = Fixture::new();
        let original = f.request();
        let first = f
            .manager
            .initialize(&f.workspace, original.clone())
            .await
            .unwrap()
            .project;
        let other_root = f.root.path().join("other");
        fs::create_dir(&other_root).unwrap();
        let other = f
            .manager
            .initialize(&other_root, f.request())
            .await
            .unwrap()
            .project;
        history(&f.manager, &first, &f.workspace, "first history").await;
        history(&f.manager, &other, &other_root, "other history").await;
        let env = environment(&f, overridden);
        let roots = integrations(&f, &env).await;
        let references = [
            fs::read(first.reference_path()).unwrap(),
            fs::read(other.reference_path()).unwrap(),
        ];
        let id = Uuid::new_v4();
        let result = f
            .manager
            .uninstall_global_with_environment(id, env.clone())
            .await
            .unwrap();
        assert_eq!(result.state, RemovalState::Removed);
        assert!(result.project_references_retained);
        assert!(result.data_loss.contains("All projects'"));
        assert!(!f.manager.home().exists());
        assert_eq!(fs::read(first.reference_path()).unwrap(), references[0]);
        assert_eq!(fs::read(other.reference_path()).unwrap(), references[1]);
        let backup = result.backup_directory.as_ref().unwrap();
        let preimages = std::path::Path::new(&backup.display).join("preimages.json");
        assert!(preimages.is_file());
        assert_eq!(
            fs::metadata(&preimages).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            fs::read_to_string(&preimages)
                .unwrap()
                .contains("private-fixture-secret")
        );
        assert_eq!(
            json(&roots.claude_registry)["oauth"]["token"],
            "private-fixture-secret"
        );
        assert!(
            json(&roots.claude_registry)["mcpServers"]
                .get("agentchat")
                .is_none()
        );
        assert_eq!(json(&roots.claude_settings)["sandbox"]["enabled"], true);
        assert_eq!(
            json(&roots.claude_settings)["hooks"]["Stop"][0]["hooks"][0]["command"],
            "foreign"
        );
        let codex = fs::read_to_string(&roots.codex_config).unwrap();
        assert!(codex.contains("approval_policy = 'on-request'"));
        assert!(codex.contains("mcp_servers.foreign"));
        assert!(codex.contains("mcp_servers.ac"));
        assert!(!codex.contains("mcp_servers.agentchat"));
        assert!(!roots.codex_skill.parent().unwrap().exists());
        assert!(!roots.claude_skill.parent().unwrap().exists());
        assert!(f.workspace.join("native ' $literal").exists());
        assert!(f.workspace.join("agentchat executable").exists());
        assert_eq!(
            f.manager
                .initialize(&f.workspace, f.request())
                .await
                .unwrap_err()
                .code,
            ErrorCode::ProjectBindingMismatch
        );
        assert_eq!(
            f.manager
                .initialize(&f.workspace, original)
                .await
                .unwrap_err()
                .code,
            ErrorCode::IdempotencyConflict
        );
        let mut fresh = f.request();
        fresh.new_project = true;
        let new = f
            .manager
            .initialize(&f.workspace, fresh)
            .await
            .unwrap()
            .project;
        assert_ne!(new.identity().id, first.identity().id);
        integrations(&f, &env).await;
        // A completed removal key returns its frozen result; later setup survives.
        assert_eq!(
            f.manager
                .uninstall_global_with_environment(id, env.clone())
                .await
                .unwrap(),
            result
        );
        assert!(new.database_path().exists());
        assert!(roots.codex_skill.exists());
        f.manager
            .uninstall_project_with_environment(&other_root, Uuid::new_v4(), env.clone())
            .await
            .unwrap();
        let recreated = f
            .manager
            .initialize(&other_root, f.request())
            .await
            .unwrap()
            .project;
        assert_ne!(recreated.identity().id, other.identity().id);
    }
}

#[tokio::test]
async fn lost_ledgers_and_entire_state_cleanup_then_fresh_preserving_integration_setup() {
    for entire in [false, true] {
        let f = Fixture::new();
        let env = environment(&f, false);
        let mut original = f.request();
        original.new_project = true;
        let project = f
            .manager
            .initialize(&f.workspace, original.clone())
            .await
            .unwrap()
            .project;
        let roots = integrations(&f, &env).await;
        if entire {
            fs::remove_dir_all(f.manager.home()).unwrap();
        } else {
            fs::remove_dir_all(f.manager.home().join("integrations")).unwrap();
        }
        // New project does not grant ownership of retained integrations.
        let mut fresh = f.request();
        fresh.new_project = true;
        f.manager.initialize(&f.workspace, fresh).await.unwrap();
        let installer = Installer::new(
            f.manager.home(),
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )
        .unwrap();
        let artifacts = roots
            .tool_artifacts(
                &[HostKind::Codex],
                &f.workspace.join("agentchat executable"),
                f.manager.home(),
            )
            .unwrap();
        assert_eq!(
            installer.install(artifacts).await.unwrap_err().code,
            ErrorCode::ConfigurationConflict
        );
        let removed = f
            .manager
            .uninstall_global_with_environment(Uuid::new_v4(), env.clone())
            .await
            .unwrap();
        assert!(removed.backup_directory.is_some());
        assert!(project.reference_path().exists());
        assert_eq!(
            f.manager
                .initialize(&f.workspace, original)
                .await
                .unwrap_err()
                .code,
            ErrorCode::IdempotencyConflict
        );
        f.manager
            .uninstall_project_with_environment(&f.workspace, Uuid::new_v4(), env.clone())
            .await
            .unwrap();
        f.manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap();
        integrations(&f, &env).await;
        assert!(roots.codex_skill.exists());
    }
}

#[tokio::test]
async fn global_failure_boundaries_recover_and_foreign_host_edits_preserve_all_history() {
    for stage in [
        RemovalStage::Prepared,
        RemovalStage::ReceiptsRetired,
        RemovalStage::ConfigurationBackedUp,
        RemovalStage::IntegrationRemoved { index: 0 },
        RemovalStage::ComponentRemoved { index: 0 },
        RemovalStage::ReceiptPublished,
        RemovalStage::Complete,
    ] {
        let f = Fixture::new();
        let env = environment(&f, false);
        let project = f
            .manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap()
            .project;
        integrations(&f, &env).await;
        let manager = f.manager.clone().with_removal_progress(Arc::new(FailOnce {
            stage,
            fired: AtomicBool::new(false),
        }));
        let id = Uuid::new_v4();
        let error = manager
            .uninstall_global_with_environment(id, env.clone())
            .await
            .unwrap_err();
        assert_eq!(error.details["reason"], "partial_removal");
        assert!(!error.to_string().contains("private-fixture-secret"));
        let result = manager
            .uninstall_global_with_environment(id, env.clone())
            .await
            .unwrap();
        assert!(result.backup_directory.is_some());
        assert!(!f.manager.home().exists());
        assert!(project.reference_path().exists());
    }
    let f = Fixture::new();
    let env = environment(&f, false);
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let roots = integrations(&f, &env).await;
    let manager = f.manager.clone().with_removal_progress(Arc::new(FailOnce {
        stage: RemovalStage::IntegrationRemoved { index: 0 },
        fired: AtomicBool::new(false),
    }));
    let id = Uuid::new_v4();
    manager
        .uninstall_global_with_environment(id, env.clone())
        .await
        .unwrap_err();
    let mut config = json(&roots.claude_registry);
    config["new-foreign-setting"] = json!(true);
    fs::write(&roots.claude_registry, config.to_string()).unwrap();
    let error = manager
        .uninstall_global_with_environment(id, env.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigurationConflict);
    assert!(project.database_path().exists());
    assert_eq!(json(&roots.claude_registry), config);
    assert_eq!(
        manager
            .uninstall_global_with_environment(Uuid::new_v4(), env)
            .await
            .unwrap_err()
            .details["reason"],
        "removal_recovery_required"
    );
}

#[tokio::test]
async fn global_activity_absence_and_destination_identity_are_bounded() {
    let f = Fixture::new();
    let env = environment(&f, false);
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let lifecycle = Lifecycle::new(f.manager.home()).unwrap();
    let activity = lifecycle.activity(project.identity().id).unwrap();
    assert_eq!(
        f.manager
            .uninstall_global_with_environment(Uuid::new_v4(), env.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert!(project.database_path().exists());
    drop(activity);
    let id = Uuid::new_v4();
    f.manager
        .uninstall_global_with_environment(id, env.clone())
        .await
        .unwrap();
    let absent = f
        .manager
        .uninstall_global_with_environment(Uuid::new_v4(), env.clone())
        .await
        .unwrap();
    assert_eq!(absent.state, RemovalState::AlreadyAbsent);
    assert!(absent.backup_directory.is_none());
    let mut changed = env;
    changed.claude_config_dir = Some(f.config.join("different"));
    assert_eq!(
        f.manager
            .uninstall_global_with_environment(id, changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
}

#[tokio::test]
async fn global_preserves_legacy_ac_hooks_and_state_with_or_without_ownership_records() {
    for lost in [false, true] {
        let f = Fixture::new();
        let env = environment(&f, false);
        let project = f
            .manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap()
            .project;
        let roots = integrations(&f, &env).await;
        let legacy = f.root.path().canonicalize().unwrap().join(".ac");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("retained-state"), b"legacy state must remain").unwrap();
        let legacy_binary = f.workspace.join("ac");
        fs::write(&legacy_binary, b"legacy artifact must remain").unwrap();
        let mut settings = json(&roots.claude_settings);
        let mut groups = Vec::new();
        for (event, mode) in [
            ("FileChanged", "notice"),
            ("Stop", "notice"),
            ("UserPromptSubmit", "final"),
        ] {
            let command = format!(
                "'{}' internal claude-input --mode {mode} --state-directory '{}' --native-executable '{}'",
                legacy_binary.display(),
                legacy.display(),
                f.workspace.join("native").display()
            );
            let member = if mode == "notice" {
                json!({"type":"command","command":command,"asyncRewake":true,"timeout":15})
            } else {
                json!({"type":"command","command":command,"timeout":10,"onFailure":"block"})
            };
            let group = if event == "FileChanged" {
                json!({"matcher":".ac-wakeup","hooks":[member]})
            } else {
                json!({"hooks":[member]})
            };
            settings["hooks"][event]
                .as_array_mut()
                .unwrap()
                .push(group.clone());
            groups.push((event, group));
        }
        fs::write(&roots.claude_settings, settings.to_string()).unwrap();
        if lost {
            fs::remove_dir_all(f.manager.home().join("integrations")).unwrap();
        }
        f.manager
            .uninstall_global_with_environment(Uuid::new_v4(), env)
            .await
            .unwrap();
        let settings = json(&roots.claude_settings);
        for (event, group) in groups {
            assert!(
                settings["hooks"][event]
                    .as_array()
                    .unwrap()
                    .contains(&group)
            );
        }
        assert_eq!(
            fs::read(legacy.join("retained-state")).unwrap(),
            b"legacy state must remain"
        );
        assert_eq!(
            fs::read(legacy_binary).unwrap(),
            b"legacy artifact must remain"
        );
        assert!(!f.manager.home().exists());
        assert!(project.reference_path().exists());
    }
}

#[tokio::test]
async fn an_aliased_hook_state_cannot_be_misclassified_as_an_independent_foreign_root() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let env = environment(&f, false);
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let roots = integrations(&f, &env).await;
    let alias = f.root.path().canonicalize().unwrap().join("state-alias");
    symlink(f.manager.home(), &alias).unwrap();
    let mut settings = json(&roots.claude_settings);
    let original = format!("--state-directory '{}'", f.manager.home().display());
    let replacement = format!("--state-directory '{}'", alias.display());
    let mut changed = false;
    for group in settings["hooks"]["Stop"].as_array_mut().unwrap() {
        for member in group["hooks"].as_array_mut().unwrap() {
            if let Some(command) = member["command"].as_str().filter(|c| c.contains(&original)) {
                member["command"] = json!(command.replace(&original, &replacement));
                changed = true;
            }
        }
    }
    assert!(changed);
    let before = settings.to_string();
    fs::write(&roots.claude_settings, &before).unwrap();
    fs::remove_dir_all(f.manager.home().join("integrations")).unwrap();
    assert_eq!(
        f.manager
            .uninstall_global_with_environment(Uuid::new_v4(), env)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read_to_string(&roots.claude_settings).unwrap(), before);
    assert!(project.database_path().exists());
    assert!(project.reference_path().exists());
}

#[tokio::test]
async fn global_rejects_host_overlap_symlinks_and_a_selected_root_containing_project_references() {
    use agentchat_runtime::project::ProjectManager;
    use std::os::unix::fs::symlink;
    for kind in ["host-overlap", "symlink", "reference-overlap"] {
        let f = Fixture::new();
        let env = environment(&f, false);
        let selected = match kind {
            "host-overlap" => {
                fs::create_dir_all(f.config.join(".agents/skills/agentchat/state")).unwrap();
                f.config.join(".agents/skills/agentchat/state")
            }
            "reference-overlap" => {
                f.manager
                    .initialize(&f.workspace, f.request())
                    .await
                    .unwrap();
                f.workspace.join(".agentchat")
            }
            _ => {
                f.manager
                    .initialize(&f.workspace, f.request())
                    .await
                    .unwrap();
                let outside = f.workspace.join("outside");
                fs::write(&outside, b"preserved").unwrap();
                symlink(&outside, f.manager.home().join("escape")).unwrap();
                f.manager.home().to_owned()
            }
        };
        let before = fs::read_dir(&selected).unwrap().count();
        let manager = ProjectManager::new(&selected).unwrap();
        let error = manager
            .uninstall_global_with_environment(Uuid::new_v4(), env)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigurationConflict);
        assert!(selected.is_dir());
        assert_eq!(fs::read_dir(&selected).unwrap().count(), before);
        if kind == "host-overlap" {
            assert!(!Lifecycle::new(manager.home()).unwrap().directory().exists());
        }
        assert!(
            !Lifecycle::new(manager.home())
                .unwrap()
                .directory()
                .join("pending-removal.json")
                .exists()
        );
    }
}
