//! Serialized creation, immutable retries, canonical binding and preserving merges.

mod support;

#[tokio::test]
async fn state_root_bytes_stay_canonical_before_and_after_creation() {
    let f = support::Fixture::new();
    let selected = f.manager.home().to_owned();
    let mut with_separator = selected.as_os_str().to_owned();
    with_separator.push("/");
    let before = agentchat_runtime::project::ProjectManager::new(&with_separator).unwrap();
    assert_eq!(before.home().as_os_str(), selected.as_os_str());
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let after = agentchat_runtime::project::ProjectManager::new(&with_separator).unwrap();
    assert_eq!(after.home().as_os_str(), selected.as_os_str());
}

#[tokio::test]
async fn transient_bound_guard_is_checked_under_the_init_lock_but_not_part_of_successful_retry_identity()
 {
    let f = support::Fixture::new();
    assert!(
        f.manager
            .resume_initialization(&f.workspace, f.request())
            .await
            .unwrap()
            .is_none()
    );
    assert!(!f.manager.home().exists());
    let first = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let mut bound = f.request();
    bound.new_project = true;
    bound.expected_project_id = Some(first.project.identity().id);
    let second = f
        .manager
        .initialize(&f.workspace, bound.clone())
        .await
        .unwrap();
    let mut retry = bound.clone();
    retry.expected_project_id = Some(second.project.identity().id);
    assert_eq!(
        f.manager
            .resume_initialization(&f.workspace, retry)
            .await
            .unwrap()
            .unwrap(),
        second
    );
    let mut stale = f.request();
    stale.new_project = true;
    stale.expected_project_id = Some(first.project.identity().id);
    assert_eq!(
        f.manager
            .initialize(&f.workspace, stale)
            .await
            .unwrap_err()
            .code,
        agentchat_core::ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(
        f.manager.discover(&f.workspace).await.unwrap().unwrap(),
        second.project
    );
}

use agentchat_core::{ErrorCode, model::HostKind, read::ChannelListCondition, time::Timestamp};
use agentchat_runtime::project::{InitActor, ProjectManager};
use std::{fs, os::unix::fs::PermissionsExt};
use support::Fixture;

#[tokio::test]
async fn repeated_and_concurrent_initialization_preserve_uuid_database_and_default_channel() {
    let f = Fixture::new();
    let request = f.request();
    let (first, second) = tokio::join!(
        f.manager.initialize(&f.workspace, request.clone()),
        f.manager.initialize(&f.workspace, request.clone())
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first, second);
    assert!(first.created);
    let repeated = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    assert!(!repeated.created);
    assert_eq!(repeated.project, first.project);
    let store = f.manager.open(&first.project).await.unwrap();
    let mut snapshot = agentchat_core::ports::Storage::snapshot(store.as_ref())
        .await
        .unwrap();
    assert_eq!(snapshot.head().get(), 2);
    assert_eq!(
        snapshot
            .channel_policy()
            .await
            .unwrap()
            .unwrap()
            .ordered_defaults(),
        ["dev"]
    );
    assert_eq!(
        snapshot
            .events(Default::default(), 10, None)
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        [
            agentchat_core::model::EventKind::ChannelCreated,
            agentchat_core::model::EventKind::ProjectChannelPolicyMaterialized
        ]
    );
    assert_eq!(
        snapshot
            .channels(
                Default::default(),
                201,
                ChannelListCondition::All,
                Timestamp::parse("2026-10-26T00:00:00.000Z").unwrap(),
            )
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(snapshot.statistics().await.unwrap().agents, 0);
    for path in [
        first.project.data_dir().to_owned(),
        first.project.reference_path().parent().unwrap().to_owned(),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    for path in [
        first.project.database_path(),
        first.project.reference_path(),
        first.project.data_dir().join("project.json"),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn ordered_defaults_preserve_omission_and_successful_retry_without_rewinding() {
    let f = Fixture::new();
    let mut configured = f.request();
    configured.default_channels = Some(vec![" Ops ".into(), "general".into(), "OPS".into()]);
    let first = f
        .manager
        .initialize(&f.workspace, configured.clone())
        .await
        .unwrap();
    assert_eq!(first.channel_policy.ordered_defaults(), ["ops", "general"]);
    assert_eq!(first.channel_policy.primary(), "ops");

    let preserved = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    assert_eq!(preserved.channel_policy, first.channel_policy);

    let mut replaced = f.request();
    replaced.default_channels = Some(vec!["dev".into()]);
    let replacement = f.manager.initialize(&f.workspace, replaced).await.unwrap();
    assert_eq!(replacement.channel_policy.ordered_defaults(), ["dev"]);

    let replay = f
        .manager
        .initialize(&f.workspace, configured.clone())
        .await
        .unwrap();
    assert_eq!(replay, first);
    let current = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    assert_eq!(current.channel_policy, replacement.channel_policy);

    let mut conflict = configured;
    conflict.default_channels = Some(vec!["different".into()]);
    assert_eq!(
        f.manager
            .initialize(&f.workspace, conflict)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
}

#[tokio::test]
async fn new_project_replays_original_result_without_rewinding_a_later_reference() {
    let f = Fixture::new();
    let first = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let mut request = f.request();
    request.new_project = true;
    let second = f
        .manager
        .initialize(&f.workspace, request.clone())
        .await
        .unwrap();
    assert_ne!(first.project.identity().id, second.project.identity().id);
    let mut later = f.request();
    later.new_project = true;
    let third = f.manager.initialize(&f.workspace, later).await.unwrap();
    assert_ne!(second.project.identity().id, third.project.identity().id);
    let reference = fs::read(third.project.reference_path()).unwrap();
    assert_eq!(
        f.manager
            .initialize(&f.workspace, request.clone())
            .await
            .unwrap(),
        second
    );
    assert_eq!(fs::read(third.project.reference_path()).unwrap(), reference);
    assert_eq!(
        f.manager.discover(&f.workspace).await.unwrap().unwrap(),
        third.project
    );
    assert!(first.project.database_path().exists());
    assert!(second.project.database_path().exists());
    assert_eq!(
        f.manager.open(&first.project).await.err().unwrap().code,
        ErrorCode::ProjectBindingMismatch
    );
    let mut different = request.clone();
    different.actor = InitActor::Conversation {
        host: HostKind::Claude,
        host_session_id: "another-exact-session".into(),
    };
    assert_eq!(
        f.manager
            .initialize(&f.workspace, different)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    let mut different = request;
    different.new_project = false;
    assert_eq!(
        f.manager
            .initialize(&f.workspace, different)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(fs::read(third.project.reference_path()).unwrap(), reference);
}

#[tokio::test]
async fn copied_reference_is_rejected_and_relocation_requires_the_old_root_to_disappear() {
    let f = Fixture::new();
    let original = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let copied = f.root.path().join("copied");
    fs::create_dir(&copied).unwrap();
    fs::create_dir(copied.join(".agentchat")).unwrap();
    fs::copy(
        original.project.reference_path(),
        copied.join(".agentchat/project.json"),
    )
    .unwrap();
    assert_eq!(
        f.manager.discover(&copied).await.unwrap_err().code,
        ErrorCode::ProjectBindingMismatch
    );
    let mut relocate = f.request();
    relocate.relocate = true;
    assert_eq!(
        f.manager
            .initialize(&copied, relocate.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(
        f.manager.discover(&f.workspace).await.unwrap().unwrap(),
        original.project
    );
    fs::remove_dir_all(&copied).unwrap();
    fs::rename(&f.workspace, &copied).unwrap();
    let db_before = fs::read(original.project.database_path()).unwrap();
    let moved = f
        .manager
        .initialize(&copied, relocate.clone())
        .await
        .unwrap();
    assert_eq!(moved.project.identity(), original.project.identity());
    assert!(!moved.created);
    assert_eq!(
        f.manager.discover(&copied).await.unwrap().unwrap(),
        moved.project
    );
    assert_eq!(
        fs::read(original.project.database_path()).unwrap(),
        db_before
    );
    assert_eq!(
        f.manager.initialize(&copied, relocate).await.unwrap(),
        moved
    );
}

#[tokio::test]
async fn git_local_exclusions_preserve_foreign_bytes_modes_and_tracked_ignore() {
    let f = Fixture::new();
    f.repository(&f.workspace);
    let ignore = f.workspace.join(".gitignore");
    fs::write(&ignore, b"tracked-choice\n").unwrap();
    f.git(&f.workspace, &["add", ".gitignore"]);
    f.git(&f.workspace, &["commit", "--quiet", "-m", "fixture ignore"]);
    let exclude = f.workspace.join(".git/info/exclude");
    let prefix = b"# Foreign comment\r\nforeign-pattern\r\n";
    fs::write(&exclude, prefix).unwrap();
    fs::set_permissions(&exclude, fs::Permissions::from_mode(0o640)).unwrap();
    let initialized = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let merged = fs::read(&exclude).unwrap();
    assert!(merged.starts_with(prefix));
    assert_eq!(
        fs::metadata(&exclude).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(fs::read(&ignore).unwrap(), b"tracked-choice\n");
    f.manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    assert_eq!(fs::read(&exclude).unwrap(), merged);
    f.git(&f.workspace, &["check-ignore", ".agentchat-wakeup"]);
    // Repository references live under Git common metadata, while a directory
    // override's exact reference is ignored without hiding foreign .agentchat content.
    let child = f.workspace.join("child");
    fs::create_dir(&child).unwrap();
    let mut request = f.request();
    request.new_project = true;
    request.scope = Some(agentchat_core::project::Scope::Directory);
    let directory = f.manager.initialize(&child, request).await.unwrap();
    f.git(
        &f.workspace,
        &["check-ignore", "child/.agentchat/project.json"],
    );
    assert_ne!(directory.project.identity(), initialized.project.identity());
    assert_eq!(fs::read(&ignore).unwrap(), b"tracked-choice\n");
}

#[tokio::test]
async fn invalid_modes_and_foreign_state_roots_preserve_existing_references() {
    let f = Fixture::new();
    let mut invalid = f.request();
    invalid.new_project = true;
    invalid.relocate = true;
    assert_eq!(
        f.manager
            .initialize(&f.workspace, invalid)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert!(!f.manager.home().exists());
    let original = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let other = ProjectManager::new(f.root.path().join("other-state")).unwrap();
    let before = fs::read(original.project.reference_path()).unwrap();
    assert_eq!(
        other.discover(&f.workspace).await.unwrap_err().code,
        ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(
        other
            .initialize(&f.workspace, f.request())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(fs::read(original.project.reference_path()).unwrap(), before);
}

#[tokio::test]
async fn missing_retained_database_and_future_reference_formats_are_never_reinitialized() {
    let f = Fixture::new();
    let original = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let reference = original.project.reference_path();
    let before = fs::read(&reference).unwrap();
    fs::remove_file(original.project.database_path()).unwrap();
    assert_eq!(
        f.manager
            .initialize(&f.workspace, f.request())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    assert!(!original.project.database_path().exists());
    assert_eq!(fs::read(&reference).unwrap(), before);
    assert!(!f.manager.home().join("init/pending.json").exists());
    let mut metadata: serde_json::Value = serde_json::from_slice(&before).unwrap();
    metadata["version"] = 2.into();
    let newer = serde_json::to_vec(&metadata).unwrap();
    fs::write(&reference, &newer).unwrap();
    let mut replacement = f.request();
    replacement.new_project = true;
    assert_eq!(
        f.manager
            .initialize(&f.workspace, replacement)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedProtocol
    );
    assert_eq!(fs::read(&reference).unwrap(), newer);
}
