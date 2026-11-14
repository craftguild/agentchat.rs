//! Actual publication failures must never unlink substituted stage data.
use super::*;
use std::os::unix::fs::MetadataExt;

fn identity(path: &Path) -> (u64, u64) {
    let m = fs::symlink_metadata(path).unwrap();
    (m.dev(), m.ino())
}
async fn exercise(kind: &str) -> bool {
    let owned = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = owned.path().canonicalize().unwrap();
    let contexts = root.join("contexts");
    directory(&contexts).unwrap();
    let target = contexts.join("published.json");
    let report = root.join("barrier");
    let resume = root.join("resume");
    let input = root.join("input.json");
    let value = json!({"original":"complete private context stage"});
    let data = serde_json::to_vec(&value).unwrap();
    fs::write(&input, &data).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "filesystem::tests::publication_writer_child",
            "--nocapture",
        ])
        .env("AGENTCHAT_HOME", &root)
        .env("AGENTCHAT_USER_CONFIG_ROOT", root.join("config"))
        .env("AGENTCHAT_PUBLICATION_CUT", "prepared")
        .env("AGENTCHAT_PUBLICATION_TARGET", &target)
        .env("AGENTCHAT_PUBLICATION_INPUT", &input)
        .env("AGENTCHAT_PUBLICATION_REPORT", &report)
        .env("AGENTCHAT_PUBLICATION_RESUME", &resume)
        .env("AGENTCHAT_PUBLICATION_EXPECT_FAILURE", "yes")
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
    let barrier = report.with_extension("prepared.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !barrier.exists() {
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "owned stage readiness");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let original: Value = serde_json::from_slice(&fs::read(&barrier).unwrap()).unwrap();
    let stage = std::path::PathBuf::from(original["stage"].as_str().unwrap());
    let id = identity(&stage);
    assert_eq!(
        id,
        (
            original["device"].as_u64().unwrap(),
            original["inode"].as_u64().unwrap()
        )
    );
    let saved = root.join("saved-original-stage");
    let alias = root.join("alias");
    let foreign = root.join("foreign-target");
    match kind {
        "unchanged" => {}
        "replace" => {
            fs::rename(&stage, &saved).unwrap();
            fs::write(&stage, b"foreign replacement stage").unwrap();
        }
        "edit" => fs::write(&stage, b"foreign bytes on the original stage inode").unwrap(),
        "alias" => fs::hard_link(&stage, &alias).unwrap(),
        "symlink" => {
            fs::rename(&stage, &saved).unwrap();
            fs::write(&foreign, b"foreign symbolic target").unwrap();
            std::os::unix::fs::symlink(&foreign, &stage).unwrap();
        }
        "parent" => {
            let moved = root.join("original-contexts");
            fs::rename(&contexts, &moved).unwrap();
            directory(&contexts).unwrap();
            fs::rename(moved.join(stage.file_name().unwrap()), &stage).unwrap();
            fs::write(contexts.join("foreign-note"), b"foreign replacement parent").unwrap();
            assert_eq!(
                identity(&stage),
                id,
                "the original inode moved to a foreign parent"
            );
        }
        _ => unreachable!(),
    }
    // The existing destination forces the real filesystem rename to fail.
    let collision = b"foreign canonical winner";
    fs::write(&target, collision).unwrap();
    let target_id = identity(&target);
    let expected = fs::read(&stage).unwrap();
    let stage_id = identity(&stage);
    let kind_before = fs::symlink_metadata(&stage).unwrap().file_type();
    fs::write(&resume, b"resume").unwrap();
    assert!(
        child.0.wait().unwrap().success(),
        "owned writer acknowledged its actual error"
    );
    assert_eq!(fs::read(&target).unwrap(), collision);
    assert_eq!(identity(&target), target_id);
    if saved.exists() {
        assert_eq!(fs::read(&saved).unwrap(), data);
    }
    if alias.exists() {
        assert_eq!(fs::read(&alias).unwrap(), data);
    }
    if foreign.exists() {
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign symbolic target");
    }
    if kind == "parent" {
        assert_eq!(
            fs::read(contexts.join("foreign-note")).unwrap(),
            b"foreign replacement parent"
        );
    }
    let preserved = fs::symlink_metadata(&stage).is_ok();
    if preserved {
        assert_eq!(identity(&stage), stage_id);
        assert_eq!(
            fs::symlink_metadata(&stage).unwrap().file_type(),
            kind_before
        );
        assert_eq!(fs::read(&stage).unwrap(), expected);
    }
    eprintln!(
        "STAGE_ERROR_CASE {}",
        json!({"kind":kind,"stage_preserved":preserved,
        "canonical_preserved":true,"actual_writer_exit":0})
    );
    preserved
}

#[tokio::test]
async fn a_real_collision_removes_only_the_complete_unchanged_original_stage() {
    assert!(
        !exercise("unchanged").await,
        "A complete unchanged owned stage survived its ordinary publication failure"
    );
}

#[tokio::test]
async fn publication_errors_preserve_replaced_edited_aliased_and_symbolic_stage_entries() {
    let mut missing = Vec::new();
    for kind in ["replace", "edit", "alias", "symlink"] {
        if !exercise(kind).await {
            missing.push(kind);
        }
    }
    assert!(
        missing.is_empty(),
        "Error cleanup removed foreign stage entries: {missing:?}"
    );
}

#[tokio::test]
async fn publication_errors_preserve_the_original_stage_moved_to_a_substituted_parent() {
    assert!(
        exercise("parent").await,
        "Error cleanup followed a replaced parent and removed the original stage from it"
    );
}
