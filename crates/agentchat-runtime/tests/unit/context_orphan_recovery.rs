//! Complete exact identity survives real writer loss and obsolete project routing.
use super::*;
use crate::{context::ContextSeed, project::InitRequest};
use agentchat_hosts::claude::{
    namespace::OwnerReaper,
    process::{BirthState, ProcessIdentity},
};
use std::{
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::ExitStatusExt,
    },
    path::PathBuf,
};

struct Crashed {
    _root: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    manager: ProjectManager,
    registry: ContextRegistry,
    context: Context,
    target: PathBuf,
    stage: PathBuf,
    bytes: Vec<u8>,
}
impl Crashed {
    async fn at(phase: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("state");
        let workspace = base.join("project");
        fs::create_dir(&workspace).unwrap();
        let manager = ProjectManager::new(&home).unwrap();
        manager
            .initialize(&workspace, InitRequest::new(Uuid::new_v4()))
            .await
            .unwrap();
        let registry = ContextRegistry::new(manager.clone());
        let context = registry
            .prepare(ContextSeed {
                host: HostKind::Claude,
                host_session_id: Uuid::new_v4().to_string(),
                host_version: "fixture-only".into(),
                os_profile: "fixture-only".into(),
                working_directory: workspace.clone(),
            })
            .await
            .unwrap();
        let contexts = home.join("contexts");
        directory(&contexts).unwrap();
        let target = contexts.join(format!("{}.json", context.context_id));
        let value = json!({"version":1,"context_id":context.context_id,
            "incarnation":context.incarnation,"project_id":context.project_id,
            "host":context.host,"host_session_id":context.host_session_id,
            "host_version":context.host_version,"os_profile":context.os_profile,
            "cwd_bytes":context.working_directory.as_os_str().as_bytes()});
        let input = base.join("input.json");
        fs::write(&input, serde_json::to_vec(&value).unwrap()).unwrap();
        let report = base.join("barrier");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "filesystem::tests::publication_writer_child",
            ])
            .env("AGENTCHAT_HOME", &home)
            .env("AGENTCHAT_USER_CONFIG_ROOT", base.join("config"))
            .env("AGENTCHAT_PUBLICATION_CUT", phase)
            .env("AGENTCHAT_PUBLICATION_TARGET", &target)
            .env("AGENTCHAT_PUBLICATION_INPUT", &input)
            .env("AGENTCHAT_PUBLICATION_REPORT", &report)
            .env_remove("AGENTCHAT_CONTEXT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for (actual, captured) in [
            ("HOME", "AGENTCHAT_EXPECTED_HOME"),
            ("CODEX_HOME", "AGENTCHAT_EXPECTED_CODEX_HOME"),
        ] {
            if let Some(value) = std::env::var_os(actual) {
                command.env(captured, value);
            } else {
                command.env_remove(captured);
            }
        }
        let mut child = OwnedChild(command.spawn().unwrap());
        let barrier = report.with_extension(format!("{phase}.json"));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !barrier.exists() {
            assert!(child.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let creator = ProcessIdentity::capture(child.0.id()).unwrap();
        assert!(creator.is_current());
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        assert_eq!(creator.birth_state(), BirthState::Gone);
        let prepared: Value =
            serde_json::from_slice(&fs::read(report.with_extension("prepared.json")).unwrap())
                .unwrap();
        let stage = PathBuf::from(prepared["stage"].as_str().unwrap());
        let bytes = fs::read(if phase == "prepared" { &stage } else { &target }).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), value);
        Self {
            _root: root,
            home,
            workspace,
            manager,
            registry,
            context,
            target,
            stage,
            bytes,
        }
    }
    fn collect(&self) -> (usize, usize) {
        let mut reaper =
            OwnerReaper::new(&self.home).with_context_validator(ContextRegistry::validate_stage);
        let mut removed = 0;
        let mut preserved = 0;
        for _ in 0..8 {
            let page = reaper.step().unwrap();
            assert!(page.visited <= 8 && page.removed <= 40 && page.directories_removed <= 1);
            removed += page.removed;
            preserved += page.preserved;
        }
        assert!(
            !self.home.join("hosts").exists(),
            "physical history recovery cannot initialize a native owner"
        );
        (removed, preserved)
    }
}

#[tokio::test]
async fn killed_context_writer_recovers_full_identity_after_project_replacement_or_cwd_loss() {
    for route in ["replaced", "gone"] {
        let f = Crashed::at("prepared").await;
        let original_inode = fs::metadata(&f.stage).unwrap().ino();
        assert!(!f.target.exists());
        assert_eq!(
            f.registry
                .descriptor(f.context.context_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::SessionRequired
        );
        if route == "replaced" {
            let mut request = InitRequest::new(Uuid::new_v4());
            request.new_project = true;
            f.manager.initialize(&f.workspace, request).await.unwrap();
            assert_ne!(
                f.manager
                    .discover(&f.workspace)
                    .await
                    .unwrap()
                    .unwrap()
                    .identity()
                    .id,
                f.context.project_id
            );
        } else {
            fs::remove_dir_all(&f.workspace).unwrap();
        }
        assert_eq!(f.collect().0, 1);
        assert!(!f.stage.exists());
        assert_eq!(fs::read(&f.target).unwrap(), f.bytes);
        assert_eq!(fs::metadata(&f.target).unwrap().ino(), original_inode);
        assert_eq!(
            f.registry.descriptor(f.context.context_id).await.unwrap(),
            f.context
        );
        assert!(
            f.registry
                .resolve(f.context.context_id, None)
                .await
                .is_err()
        );
        assert_eq!(fs::read_dir(f.home.join("staging")).unwrap().count(), 0);
        assert_eq!(f.collect().0, 0);
        assert_eq!(fs::read(&f.target).unwrap(), f.bytes);
    }
}

#[tokio::test]
async fn killed_published_context_writer_retires_only_its_pending_origin() {
    let f = Crashed::at("published").await;
    assert!(!f.stage.exists());
    let before = fs::metadata(&f.target).unwrap().ino();
    assert_eq!(
        f.registry.descriptor(f.context.context_id).await.unwrap(),
        f.context
    );
    assert_eq!(f.collect().0, 1);
    assert_eq!(fs::read(&f.target).unwrap(), f.bytes);
    assert_eq!(fs::metadata(&f.target).unwrap().ino(), before);
    assert_eq!(fs::metadata(&f.target).unwrap().nlink(), 1);
}

#[tokio::test]
async fn exact_existing_winner_keeps_its_inode_and_only_the_complete_duplicate_is_retired() {
    let f = Crashed::at("prepared").await;
    fs::write(&f.target, &f.bytes).unwrap();
    fs::set_permissions(&f.target, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::metadata(&f.target).unwrap().ino();
    assert_eq!(f.collect().0, 2);
    assert!(!f.stage.exists());
    assert_eq!(fs::metadata(&f.target).unwrap().ino(), before);
    assert_eq!(fs::read(&f.target).unwrap(), f.bytes);
    assert_eq!(
        f.registry.descriptor(f.context.context_id).await.unwrap(),
        f.context
    );
}

#[tokio::test]
async fn a_different_canonical_winner_preserves_both_full_descriptors() {
    let f = Crashed::at("prepared").await;
    let mut other: Value = serde_json::from_slice(&f.bytes).unwrap();
    other["incarnation"] = Uuid::new_v4().to_string().into();
    let bytes = serde_json::to_vec(&other).unwrap();
    fs::write(&f.target, &bytes).unwrap();
    fs::set_permissions(&f.target, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::metadata(&f.target).unwrap().ino();
    let counts = f.collect();
    assert_eq!(counts.0, 0);
    assert!(counts.1 > 0);
    assert_eq!(fs::read(&f.stage).unwrap(), f.bytes);
    assert_eq!(fs::read(&f.target).unwrap(), bytes);
    assert_eq!(fs::metadata(&f.target).unwrap().ino(), before);
    assert_eq!(fs::read_dir(f.home.join("staging")).unwrap().count(), 1);
}

#[tokio::test]
async fn context_validation_is_typed_bounded_and_preserves_every_reference() {
    let f = Crashed::at("prepared").await;
    let name = f.target.file_name().unwrap();
    ContextRegistry::validate_stage(&f.bytes, name).unwrap();
    let value: Value = serde_json::from_slice(&f.bytes).unwrap();
    for (key, bad) in [
        ("version", json!(2)),
        ("incarnation", json!(Uuid::nil())),
        ("project_id", json!(Uuid::nil())),
        ("cwd_bytes", json!([0])),
        ("unexpected", json!(true)),
    ] {
        let mut changed = value.clone();
        changed[key] = bad;
        assert!(
            ContextRegistry::validate_stage(&serde_json::to_vec(&changed).unwrap(), name).is_err(),
            "{key}"
        );
    }
    assert!(
        ContextRegistry::validate_stage(&f.bytes, format!("{}.json", Uuid::new_v4()).as_ref())
            .is_err()
    );
    assert!(ContextRegistry::validate_stage(&vec![b' '; 128 * 1024 + 1], name).is_err());
    assert_eq!(fs::read(&f.stage).unwrap(), f.bytes);
    assert!(!f.target.exists());
}

#[tokio::test]
async fn changed_or_aliased_context_stages_and_substituted_parents_keep_full_history() {
    for change in ["replace", "edit", "alias", "parent"] {
        let f = Crashed::at("prepared").await;
        let saved = f.home.join("original-stage");
        let moved = f.home.join("original-contexts");
        match change {
            "replace" => {
                fs::rename(&f.stage, &saved).unwrap();
                fs::write(&f.stage, &f.bytes).unwrap();
            }
            "edit" => {
                fs::write(&f.stage, &f.bytes).unwrap();
            }
            "alias" => fs::hard_link(&f.stage, &saved).unwrap(),
            "parent" => {
                fs::rename(f.target.parent().unwrap(), &moved).unwrap();
                directory(f.target.parent().unwrap()).unwrap();
                fs::write(
                    f.target.parent().unwrap().join("foreign-note"),
                    b"foreign parent",
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        if change == "replace" {
            fs::set_permissions(&f.stage, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let counts = f.collect();
        assert_eq!(counts.0, 0, "{change}");
        assert!(counts.1 > 0);
        assert!(!f.target.exists());
        let original = if change == "parent" {
            moved.join(f.stage.file_name().unwrap())
        } else if saved.exists() {
            saved.clone()
        } else {
            f.stage.clone()
        };
        assert_eq!(fs::read(original).unwrap(), f.bytes);
        if change == "parent" {
            assert_eq!(
                fs::read(f.target.parent().unwrap().join("foreign-note")).unwrap(),
                b"foreign parent"
            );
        }
        assert_eq!(fs::read_dir(f.home.join("staging")).unwrap().count(), 1);
    }
}

#[tokio::test]
async fn canonical_symlinks_aliases_directories_and_invalid_bodies_never_authorize_duplicate_discard()
 {
    for kind in ["symlink", "alias", "directory", "malformed"] {
        let f = Crashed::at("prepared").await;
        let other = f.home.join("foreign-canonical");
        if kind == "directory" {
            fs::create_dir(&f.target).unwrap();
        } else {
            fs::write(&other, &f.bytes).unwrap();
            fs::set_permissions(&other, fs::Permissions::from_mode(0o600)).unwrap();
            match kind {
                "symlink" => std::os::unix::fs::symlink(&other, &f.target).unwrap(),
                "alias" => fs::hard_link(&other, &f.target).unwrap(),
                "malformed" => {
                    fs::write(&f.target, b"incomplete foreign descriptor").unwrap();
                    fs::set_permissions(&f.target, fs::Permissions::from_mode(0o600)).unwrap();
                }
                _ => unreachable!(),
            }
        }
        let before = fs::symlink_metadata(&f.target).unwrap().ino();
        let counts = f.collect();
        assert_eq!(counts.0, 0, "{kind}");
        assert!(counts.1 > 0);
        assert_eq!(fs::read(&f.stage).unwrap(), f.bytes);
        assert_eq!(fs::symlink_metadata(&f.target).unwrap().ino(), before);
        if other.exists() {
            assert_eq!(fs::read(other).unwrap(), f.bytes);
        }
        if kind == "malformed" {
            assert_eq!(
                fs::read(&f.target).unwrap(),
                b"incomplete foreign descriptor"
            );
        }
        assert_eq!(fs::read_dir(f.home.join("staging")).unwrap().count(), 1);
    }
}

#[tokio::test]
async fn missing_or_changed_published_history_keeps_its_original_pending_witness() {
    for kind in ["missing", "changed", "symlink"] {
        let f = Crashed::at("published").await;
        assert!(!f.stage.exists());
        if kind == "changed" {
            let mut other: Value = serde_json::from_slice(&f.bytes).unwrap();
            other["incarnation"] = Uuid::new_v4().to_string().into();
            fs::write(&f.target, serde_json::to_vec(&other).unwrap()).unwrap();
        } else {
            fs::remove_file(&f.target).unwrap();
            if kind == "symlink" {
                let foreign = f.home.join("foreign-history");
                fs::write(&foreign, &f.bytes).unwrap();
                fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
                std::os::unix::fs::symlink(foreign, &f.target).unwrap();
            }
        }
        let before = fs::symlink_metadata(&f.target).ok().map(|m| m.ino());
        let counts = f.collect();
        assert_eq!(counts.0, 0, "{kind}");
        assert!(counts.1 > 0);
        assert_eq!(
            fs::symlink_metadata(&f.target).ok().map(|m| m.ino()),
            before
        );
        assert_eq!(fs::read_dir(f.home.join("staging")).unwrap().count(), 1);
    }
}

#[tokio::test]
async fn absent_context_stage_requires_the_runtime_policy_before_witness_retirement() {
    let f = Crashed::at("published").await;
    let page = agentchat_hosts::staging::OriginReaper::new(&f.home)
        .step()
        .unwrap();
    assert_eq!((page.removed, page.pending_policy), (0, 1));
    assert_eq!(fs::read(&f.target).unwrap(), f.bytes);
    assert_eq!(fs::read_dir(f.home.join("staging")).unwrap().count(), 1);
    assert_eq!(f.collect().0, 1);
}

#[tokio::test]
async fn a_missing_origin_index_never_adopts_legacy_context_prefixes() {
    let f = Crashed::at("prepared").await;
    // Every removed entry belongs to this stopped fixture. Lost proof does not
    // grant authority to adopt a parseable body or repair/delete its old prefix.
    fs::remove_dir_all(f.home.join("staging")).unwrap();
    assert_eq!(f.collect().0, 0);
    assert!(!f.home.join("staging").exists());
    assert_eq!(fs::read(&f.stage).unwrap(), f.bytes);
    assert!(!f.target.exists());
    assert_eq!(
        f.registry
            .descriptor(f.context.context_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
}
