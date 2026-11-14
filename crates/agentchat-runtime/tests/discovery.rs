//! Real Git/worktree boundaries and read-only UUID discovery.

mod support;

use agentchat_core::{ErrorCode, project::Scope, read::ChannelListCondition, time::Timestamp};
use agentchat_runtime::project::ProjectManager;
use std::{
    ffi::OsString,
    fs,
    os::unix::{ffi::OsStringExt, fs::symlink},
};
use support::Fixture;

#[tokio::test]
async fn uninitialized_lookup_and_symlink_access_do_not_create_state_or_participation() {
    let f = Fixture::new();
    assert!(f.manager.discover(&f.workspace).await.unwrap().is_none());
    assert!(!f.manager.home().exists());
    assert!(!f.workspace.join(".agentchat").exists());
    let initialized = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    assert_eq!(initialized.project.identity().scope, Scope::Directory);
    let child = f.workspace.join("deep/child");
    fs::create_dir_all(&child).unwrap();
    let alias = f.root.path().join("alias");
    symlink(&f.workspace, &alias).unwrap();
    assert_eq!(
        f.manager
            .discover(alias.join("deep/child"))
            .await
            .unwrap()
            .unwrap(),
        initialized.project
    );
    let store = f.manager.open(&initialized.project).await.unwrap();
    let mut snapshot = agentchat_core::ports::Storage::snapshot(store.as_ref())
        .await
        .unwrap();
    let stats = snapshot.statistics().await.unwrap();
    assert_eq!(stats.agents, 0);
    assert_eq!(stats.joined, 0);
    assert_eq!(stats.channels, 1);
    assert_eq!(stats.pending_deliveries, 0);
    assert_eq!(
        snapshot
            .channels(
                Default::default(),
                201,
                ChannelListCondition::All,
                Timestamp::parse("2026-10-26T00:00:00.000Z").unwrap(),
            )
            .await
            .unwrap()[0]
            .channel
            .name,
        "dev"
    );
    assert!(f.config.read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn linked_worktrees_share_common_directory_but_nested_repositories_stop_inheritance() {
    let f = Fixture::new();
    f.repository(&f.workspace);
    let first = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    assert_eq!(first.project.identity().scope, Scope::Repository);
    let worktree = f.root.path().join("linked");
    f.git(
        &f.workspace,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    assert!(worktree.join(".git").is_file());
    let second = f.manager.discover(&worktree).await.unwrap().unwrap();
    assert_eq!(first.project, second);
    let nested = worktree.join("nested");
    f.repository(&nested);
    assert!(f.manager.discover(&nested).await.unwrap().is_none());
    let nested_project = f.manager.initialize(&nested, f.request()).await.unwrap();
    assert_ne!(
        nested_project.project.identity().id,
        first.project.identity().id
    );
    assert!(
        nested_project
            .project
            .root()
            .starts_with(nested.canonicalize().unwrap())
    );
    let clone = f.root.path().join("clone");
    f.git(
        f.root.path(),
        &[
            "clone",
            "--quiet",
            "--local",
            "--no-hardlinks",
            f.workspace.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    assert!(f.manager.discover(&clone).await.unwrap().is_none());
    let cloned = f.manager.initialize(&clone, f.request()).await.unwrap();
    assert_ne!(cloned.project.identity().id, first.project.identity().id);
}

#[tokio::test]
async fn directory_override_is_explicit_and_repository_replacement_removes_only_that_override() {
    let f = Fixture::new();
    f.repository(&f.workspace);
    let repository = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let sub = f.workspace.join("sub");
    fs::create_dir(&sub).unwrap();
    let mut override_request = f.request();
    override_request.scope = Some(Scope::Directory);
    assert_eq!(
        f.manager
            .initialize(&sub, override_request.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ScopeConflict
    );
    override_request.new_project = true;
    let directory = f.manager.initialize(&sub, override_request).await.unwrap();
    assert_eq!(directory.project.identity().scope, Scope::Directory);
    assert_ne!(
        directory.project.identity().id,
        repository.project.identity().id
    );
    assert_eq!(
        f.manager.discover(&sub).await.unwrap().unwrap(),
        directory.project
    );
    assert_eq!(
        f.manager.discover(&f.workspace).await.unwrap().unwrap(),
        repository.project
    );
    let mut replacement = f.request();
    replacement.scope = Some(Scope::Repository);
    replacement.new_project = true;
    let replaced = f.manager.initialize(&sub, replacement).await.unwrap();
    assert_eq!(replaced.project.identity().scope, Scope::Repository);
    assert_ne!(
        replaced.project.identity().id,
        repository.project.identity().id
    );
    assert!(!directory.project.reference_path().exists());
    assert!(directory.project.database_path().exists());
    assert!(repository.project.database_path().exists());
    assert_eq!(
        f.manager.discover(&sub).await.unwrap().unwrap(),
        replaced.project
    );
}

#[tokio::test]
async fn native_path_bytes_and_terminal_newlines_round_trip_without_loss() {
    let f = Fixture::new();
    let name = OsString::from_vec("native-日本語\n".as_bytes().to_vec());
    let path = f.root.path().join(name);
    fs::create_dir(&path).unwrap();
    f.repository(&path);
    let initialized = f.manager.initialize(&path, f.request()).await.unwrap();
    assert_eq!(
        f.manager.discover(&path).await.unwrap().unwrap(),
        initialized.project
    );
    let manager = ProjectManager::new(f.manager.home()).unwrap();
    assert_eq!(
        manager.discover(&path).await.unwrap().unwrap(),
        initialized.project
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_non_utf8_native_path_bytes_round_trip_without_loss() {
    let f = Fixture::new();
    let path = f
        .root
        .path()
        .join(OsString::from_vec(b"native-\xff\n".to_vec()));
    fs::create_dir(&path).unwrap();
    f.repository(&path);
    let initialized = f.manager.initialize(&path, f.request()).await.unwrap();
    assert_eq!(
        f.manager.discover(&path).await.unwrap().unwrap(),
        initialized.project
    );
}

#[tokio::test]
async fn malformed_repository_or_foreign_reference_never_falls_back_to_initialization() {
    let f = Fixture::new();
    fs::write(f.workspace.join(".git"), b"foreign invalid gitfile").unwrap();
    assert_eq!(
        f.manager.discover(&f.workspace).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert!(!f.manager.home().exists());
    fs::remove_file(f.workspace.join(".git")).unwrap();
    fs::create_dir(f.workspace.join(".agentchat")).unwrap();
    let reference = f.workspace.join(".agentchat/project.json");
    fs::write(&reference, b"foreign non-json bytes").unwrap();
    let before = fs::read(&reference).unwrap();
    assert_eq!(
        f.manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(fs::read(&reference).unwrap(), before);
    assert!(!f.manager.home().join("init/pending.json").exists());
}

#[tokio::test]
async fn nested_worktree_and_bare_repositories_block_an_outer_directory_reference() {
    let f = Fixture::new();
    let outer = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    assert_eq!(outer.project.identity().scope, Scope::Directory);
    let inner = f.workspace.join("inner");
    f.repository(&inner);
    assert!(f.manager.discover(&inner).await.unwrap().is_none());
    let bare = f.workspace.join("bare.git");
    fs::create_dir(&bare).unwrap();
    f.git(&bare, &["init", "--quiet", "--bare"]);
    assert!(f.manager.discover(&bare).await.unwrap().is_none());
    assert_eq!(
        f.manager.discover(&f.workspace).await.unwrap().unwrap(),
        outer.project
    );
}
