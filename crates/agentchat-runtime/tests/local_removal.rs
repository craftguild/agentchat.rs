//! Destructive fixtures use only explicit temporary project/state/config roots.
mod support;

use agentchat_core::{Error, ErrorCode, Result, project::Scope};
use agentchat_hosts::{lifecycle::Lifecycle, setup::Environment};
use agentchat_protocol::removal::RemovalState;
use agentchat_runtime::{
    project::{InitRequest, ProjectManager},
    removal::{RemovalProgress, RemovalStage},
};
use std::{
    fs,
    os::unix::fs::symlink,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use support::Fixture;
use uuid::Uuid;

#[tokio::test]
async fn local_removal_deletes_only_selected_shared_descriptors_and_stale_publication_cannot_restore_state()
 {
    use agentchat_runtime::context::ContextRegistry;
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let other_root = f.root.path().join("descriptor-other");
    fs::create_dir(&other_root).unwrap();
    f.manager
        .initialize(&other_root, f.request())
        .await
        .unwrap();
    let registry = ContextRegistry::new(f.manager.clone());
    let selected = registry
        .issue(support::runtime::seed(&f, "selected"))
        .await
        .unwrap();
    let mut other_seed = support::runtime::seed(&f, "other");
    other_seed.working_directory = other_root;
    let other = registry.issue(other_seed).await.unwrap();
    let selected_path = f
        .manager
        .home()
        .join("contexts")
        .join(format!("{}.json", selected.context_id));
    let other_path = f
        .manager
        .home()
        .join("contexts")
        .join(format!("{}.json", other.context_id));
    let retained = fs::read(&other_path).unwrap();
    f.manager
        .uninstall_project_with_environment(&f.workspace, Uuid::new_v4(), environment(&f))
        .await
        .unwrap();
    assert!(!selected_path.exists());
    assert_eq!(fs::read(&other_path).unwrap(), retained);
    assert!(registry.publish(selected).await.is_err());
    assert!(f.manager.open(&project).await.is_err());
    assert!(!project.data_dir().exists());
    assert!(!project.reference_path().exists());
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
                "Injected removal interruption.",
            ));
        }
        Ok(())
    }
}
fn fail(stage: RemovalStage) -> Arc<FailOnce> {
    Arc::new(FailOnce {
        stage,
        fired: AtomicBool::new(false),
    })
}
fn environment(f: &Fixture) -> Environment {
    Environment {
        home: Some(f.root.path().join("fake-home")),
        user_config_root: Some(f.config.clone()),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_stale_reference_directory_cannot_authorize_removal_of_another_override_state_root() {
    let f = Fixture::new();
    let other = ProjectManager::new(f.workspace.join(".agentchat")).unwrap();
    let workspace = f.root.path().join("independent-workspace");
    fs::create_dir(&workspace).unwrap();
    let project = other
        .initialize(&workspace, f.request())
        .await
        .unwrap()
        .project;
    let reference = fs::read(project.reference_path()).unwrap();
    fs::write(other.home().join("project.json"), &reference).unwrap();
    let alternate = ProjectManager::new(f.root.path().join("absent-selected-state")).unwrap();
    let error = alternate
        .uninstall_project_with_environment(&f.workspace, Uuid::new_v4(), environment(&f))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigurationConflict);
    assert!(project.database_path().exists());
    assert_eq!(fs::read(project.reference_path()).unwrap(), reference);
    assert_eq!(
        fs::read(other.home().join("project.json")).unwrap(),
        reference
    );
    assert!(!alternate.home().exists());
}

#[tokio::test]
async fn a_reference_directory_overlapping_a_host_profile_preserves_configuration_and_history() {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let directory = project.reference_path().parent().unwrap().to_owned();
    let settings = directory.join("settings.json");
    fs::write(&settings, b"{\"foreign_credential\":\"fixture-only\"}").unwrap();
    let env = Environment {
        home: Some(f.root.path().join("fake-home")),
        claude_config_dir: Some(directory.to_owned()),
        ..Default::default()
    };
    let error = f
        .manager
        .uninstall_project_with_environment(&f.workspace, Uuid::new_v4(), env)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigurationConflict);
    assert!(project.database_path().exists());
    assert!(project.reference_path().exists());
    assert_eq!(
        fs::read(settings).unwrap(),
        b"{\"foreign_credential\":\"fixture-only\"}"
    );
}

#[tokio::test]
async fn lost_entire_state_does_not_allow_an_old_successful_new_project_key_to_reverse_removal() {
    let f = Fixture::new();
    let mut original = f.request();
    original.new_project = true;
    let project = f
        .manager
        .initialize(&f.workspace, original.clone())
        .await
        .unwrap()
        .project;
    fs::remove_dir_all(f.manager.home()).unwrap();
    assert_eq!(
        f.manager
            .initialize(&f.workspace, original.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    f.manager
        .uninstall_project(&f.workspace, Uuid::new_v4())
        .await
        .unwrap();
    assert!(!project.reference_path().exists());
    let error = f
        .manager
        .initialize(&f.workspace, original)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdempotencyConflict);
    assert_eq!(error.details["reason"], "installation_request_retired");
    let fresh = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    assert_ne!(fresh.identity().id, project.identity().id);
}

#[tokio::test]
async fn project_removal_preserves_other_history_integrations_and_future_installations() {
    let f = Fixture::new();
    let original = InitRequest::new(Uuid::new_v4());
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
    let retained = fs::read(other.database_path()).unwrap();
    fs::write(f.config.join("foreign-auth"), b"global-secret-never-log").unwrap();
    let id = Uuid::new_v4();
    let removed = f.manager.uninstall_project(&f.workspace, id).await.unwrap();
    assert_eq!(removed.state, RemovalState::Removed);
    assert_eq!(removed.project_id, Some(first.identity().id));
    assert!(!first.data_dir().exists());
    assert!(!first.reference_path().parent().unwrap().exists());
    assert_eq!(fs::read(other.database_path()).unwrap(), retained);
    assert_eq!(
        fs::read(f.config.join("foreign-auth")).unwrap(),
        b"global-secret-never-log"
    );
    assert_eq!(
        f.manager
            .initialize(&f.workspace, original)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    let fresh = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    assert_ne!(fresh.identity().id, first.identity().id);
    assert_eq!(
        f.manager.uninstall_project(&f.workspace, id).await.unwrap(),
        removed
    );
    assert!(fresh.database_path().exists());
    assert!(fresh.reference_path().exists());
}

#[tokio::test]
async fn linked_repository_removal_preserves_nested_repositories_and_directory_overrides() {
    let f = Fixture::new();
    f.repository(&f.workspace);
    let repo = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let linked = f.root.path().join("linked");
    f.git(
        &f.workspace,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let inner = f.workspace.join("inner");
    fs::create_dir(&inner).unwrap();
    let mut request = f.request();
    request.new_project = true;
    request.scope = Some(Scope::Directory);
    let directory = f.manager.initialize(&inner, request).await.unwrap().project;
    let nested = f.workspace.join("nested");
    f.repository(&nested);
    let nested_project = f
        .manager
        .initialize(&nested, f.request())
        .await
        .unwrap()
        .project;
    let removed = f
        .manager
        .uninstall_project(&linked, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(removed.project_id, Some(repo.identity().id));
    assert!(!repo.data_dir().exists());
    assert!(!repo.reference_path().exists());
    for project in [&directory, &nested_project] {
        assert!(project.database_path().exists());
        assert!(project.reference_path().exists());
    }
    assert!(f.manager.discover(&linked).await.unwrap().is_none());
    f.manager
        .uninstall_project(&inner, Uuid::new_v4())
        .await
        .unwrap();
    assert!(!directory.data_dir().exists());
    assert!(nested_project.database_path().exists());
}

#[tokio::test]
async fn stale_reference_is_removable_and_absence_does_not_scan_retained_uuids_by_path() {
    let f = Fixture::new();
    let retained = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let mut replace = f.request();
    replace.new_project = true;
    let selected = f
        .manager
        .initialize(&f.workspace, replace)
        .await
        .unwrap()
        .project;
    fs::remove_dir_all(selected.data_dir()).unwrap();
    f.manager
        .uninstall_project(&f.workspace, Uuid::new_v4())
        .await
        .unwrap();
    assert!(retained.database_path().exists());
    assert!(!selected.reference_path().parent().unwrap().exists());
    let absent = f
        .manager
        .uninstall_project(&f.workspace, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(absent.state, RemovalState::AlreadyAbsent);
    assert_eq!(absent.project_id, None);
    assert!(retained.database_path().exists());
    let fresh = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    assert_ne!(fresh.identity().id, retained.identity().id);
}

#[tokio::test]
async fn copied_references_and_present_missing_or_foreign_bindings_preserve_all_targets() {
    for variant in ["copied", "missing", "foreign"] {
        let f = Fixture::new();
        let project = f
            .manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap()
            .project;
        let reference = fs::read(project.reference_path()).unwrap();
        let mut selected = f.workspace.clone();
        match variant {
            "copied" => {
                selected = f.root.path().join("copied");
                fs::create_dir(&selected).unwrap();
                fs::create_dir(selected.join(".agentchat")).unwrap();
                fs::copy(
                    project.reference_path(),
                    selected.join(".agentchat/project.json"),
                )
                .unwrap();
            }
            "missing" => fs::remove_file(project.data_dir().join("project.json")).unwrap(),
            _ => {
                let mut binding: serde_json::Value = serde_json::from_slice(
                    &fs::read(project.data_dir().join("project.json")).unwrap(),
                )
                .unwrap();
                binding["identity"]["id"] = serde_json::json!(Uuid::new_v4());
                fs::write(
                    project.data_dir().join("project.json"),
                    serde_json::to_vec(&binding).unwrap(),
                )
                .unwrap();
            }
        }
        let error = f
            .manager
            .uninstall_project(&selected, Uuid::new_v4())
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::ProjectBindingMismatch,
            "{variant}: {error}"
        );
        assert!(project.database_path().exists());
        assert_eq!(fs::read(project.reference_path()).unwrap(), reference);
    }
}

#[tokio::test]
async fn every_durable_local_boundary_recovers_same_request_and_blocks_stale_setup() {
    for stage in [
        RemovalStage::Prepared,
        RemovalStage::ReceiptsRetired,
        RemovalStage::ComponentRemoved { index: 0 },
        RemovalStage::NodeRemoved {
            component: 1,
            node: 0,
        },
        RemovalStage::ComponentRemoved { index: 1 },
        RemovalStage::ReceiptPublished,
    ] {
        let f = Fixture::new();
        let install = f.request();
        let project = f
            .manager
            .initialize(&f.workspace, install.clone())
            .await
            .unwrap()
            .project;
        let sink = fail(stage);
        let manager = f.manager.clone().with_removal_progress(sink.clone());
        let id = Uuid::new_v4();
        let error = manager
            .uninstall_project(&f.workspace, id)
            .await
            .unwrap_err();
        assert!(sink.fired.load(Ordering::SeqCst));
        assert_eq!(error.details["reason"], "partial_removal");
        if stage != RemovalStage::ReceiptPublished {
            let blocked = f
                .manager
                .initialize(&f.workspace, f.request())
                .await
                .unwrap_err();
            assert_eq!(blocked.details["reason"], "removal_recovery_required");
        }
        let done = f.manager.uninstall_project(&f.workspace, id).await.unwrap();
        assert_eq!(done.project_id, Some(project.identity().id));
        assert!(!project.data_dir().exists());
        assert!(!project.reference_path().parent().unwrap().exists());
        assert_eq!(
            f.manager.uninstall_project(&f.workspace, id).await.unwrap(),
            done
        );
        assert_eq!(
            f.manager
                .initialize(&f.workspace, install)
                .await
                .unwrap_err()
                .code,
            ErrorCode::IdempotencyConflict
        );
    }
}

#[tokio::test]
async fn changed_tree_preimages_are_not_deleted_and_pending_request_identity_is_exact() {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let manager = f
        .manager
        .clone()
        .with_removal_progress(fail(RemovalStage::Prepared));
    let id = Uuid::new_v4();
    manager
        .uninstall_project(&f.workspace, id)
        .await
        .unwrap_err();
    let foreign = project.data_dir().join("foreign-arrival");
    fs::write(&foreign, b"retain new data").unwrap();
    let error = f
        .manager
        .uninstall_project(&f.workspace, id)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigurationConflict);
    assert_eq!(fs::read(&foreign).unwrap(), b"retain new data");
    assert!(project.reference_path().exists());
    assert!(project.database_path().exists());
    let other = f.root.path().join("other");
    fs::create_dir(&other).unwrap();
    assert_eq!(
        f.manager
            .uninstall_project(&other, id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    fs::remove_file(&foreign).unwrap();
    f.manager.uninstall_project(&f.workspace, id).await.unwrap();
}

#[tokio::test]
async fn relevant_activity_refuses_removal_but_other_activity_does_not_block_local_or_absent_scope()
{
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let lifecycle = Lifecycle::new(f.manager.home()).unwrap();
    let owner = lifecycle.activity(project.identity().id).unwrap();
    assert_eq!(
        f.manager
            .uninstall_project(&f.workspace, Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert!(project.database_path().exists());
    drop(owner);
    let unrelated = lifecycle.activity(Uuid::new_v4()).unwrap();
    f.manager
        .uninstall_project(&f.workspace, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        f.manager
            .uninstall_project(&f.workspace, Uuid::new_v4())
            .await
            .unwrap()
            .state,
        RemovalState::AlreadyAbsent
    );
    drop(unrelated);
}

#[tokio::test]
async fn symlink_escape_and_dangerous_selected_roots_are_refused_without_mutation() {
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let foreign = f.root.path().join("foreign");
    fs::write(&foreign, b"private foreign bytes").unwrap();
    symlink(&foreign, project.data_dir().join("escape")).unwrap();
    assert_eq!(
        f.manager
            .uninstall_project(&f.workspace, Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(&foreign).unwrap(), b"private foreign bytes");
    assert!(project.reference_path().exists());
    let fake_home = f.root.path().join("fake-home");
    fs::create_dir(&fake_home).unwrap();
    for dangerous in [fake_home, f.config.clone()] {
        let manager = ProjectManager::new(&dangerous).unwrap();
        assert_eq!(
            manager
                .uninstall_project_with_environment(&f.workspace, Uuid::new_v4(), environment(&f))
                .await
                .unwrap_err()
                .code,
            ErrorCode::ConfigurationConflict
        );
    }
    assert!(project.database_path().exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn local_removal_journal_preserves_native_non_utf8_paths() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let f = Fixture::new();
    let cwd = f
        .workspace
        .join(OsString::from_vec(b"native-\xff".to_vec()));
    fs::create_dir(&cwd).unwrap();
    let manager = f.manager.clone();
    let project = manager.initialize(&cwd, f.request()).await.unwrap().project;
    let id = Uuid::new_v4();
    let result = manager.uninstall_project(&cwd, id).await.unwrap();
    assert_eq!(result.project_id, Some(project.identity().id));
    assert!(!project.data_dir().exists());
    assert_eq!(manager.uninstall_project(&cwd, id).await.unwrap(), result);

    // A stale reference under an alternate selected root needs no SQLite open.
    // That root still travels losslessly through intent/result recovery while
    // the original override's database stays outside removal authority.
    let other = Fixture::new();
    let retained = other
        .manager
        .initialize(&other.workspace, other.request())
        .await
        .unwrap()
        .project;
    let parent = other
        .root
        .path()
        .join(OsString::from_vec(b"native-state-parent-\xff".to_vec()));
    fs::create_dir(&parent).unwrap();
    let alternate = ProjectManager::new(parent.join("state"))
        .unwrap()
        .with_removal_progress(fail(RemovalStage::Prepared));
    let id = Uuid::new_v4();
    let interrupted = alternate
        .uninstall_project(&other.workspace, id)
        .await
        .unwrap_err();
    assert_eq!(interrupted.details["reason"], "partial_removal");
    let different = alternate
        .uninstall_project(&other.workspace, Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(different.details["reason"], "removal_recovery_required");
    let setup = alternate
        .initialize(&other.workspace, other.request())
        .await
        .unwrap_err();
    assert_eq!(setup.details["reason"], "removal_recovery_required");
    let recovery = Lifecycle::new(alternate.home())
        .unwrap()
        .directory()
        .as_os_str()
        .as_encoded_bytes()
        .to_vec();
    assert_eq!(
        different.details["recovery_directory"]["native_bytes"],
        serde_json::json!(recovery)
    );
    assert_eq!(
        setup.details["recovery_directory"]["native_bytes"],
        serde_json::json!(recovery)
    );
    let result = alternate
        .uninstall_project(&other.workspace, id)
        .await
        .unwrap();
    assert!(retained.database_path().exists());
    assert!(!retained.reference_path().exists());
    assert_eq!(
        alternate
            .uninstall_project(&other.workspace, id)
            .await
            .unwrap(),
        result
    );
}
