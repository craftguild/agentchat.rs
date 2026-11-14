//! Actual filesystem merges, ownership collisions, bounds and serialized retry.
mod support;
use agentchat_core::ErrorCode;
use agentchat_hosts::install::{Artifact, Content, InstallProgress, InstallStage, Installer};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::Arc,
};
use support::Fixture;

#[tokio::test]
async fn merges_preserve_foreign_settings_comments_numbers_and_repeat_exactly() {
    let f = Fixture::new();
    f.seed();
    fs::set_permissions(
        f.config.join(".claude.json"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    assert_eq!(
        f.installer
            .install(f.artifacts(1))
            .await
            .unwrap()
            .changed_files,
        4
    );
    let toml = fs::read_to_string(f.config.join(".codex/config.toml")).unwrap();
    assert!(toml.contains("# Keep this comment exactly\nmodel = 'foreign-model' # keep model"));
    assert!(toml.contains("command = 'foreign-tool' # keep command"));
    let parsed = toml.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        parsed["mcp_servers"]["agentchat"]["startup_timeout_sec"].as_integer(),
        Some(10)
    );
    assert_eq!(
        f.json(".claude.json")["projects"]["/foreign"]["hasTrustDialogAccepted"],
        false
    );
    assert_eq!(
        f.json(".claude.json")["big"].to_string(),
        "1234567890123456789012345678901234567890"
    );
    assert_eq!(
        f.json(".claude/settings.json")["permissions"]["defaultMode"],
        "plan"
    );
    assert_eq!(
        f.json(".claude/settings.json")["watchPaths"],
        json!(["foreign.log"])
    );
    assert_eq!(
        f.json(".claude/settings.json")["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fs::metadata(f.config.join(".claude.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    let snapshot = fs::read(f.config.join(".claude/settings.json")).unwrap();
    let report = f.installer.install(f.artifacts(1)).await.unwrap();
    assert_eq!(report.changed_files, 0);
    assert!(!report.recovered);
    assert_eq!(
        fs::read(f.config.join(".claude/settings.json")).unwrap(),
        snapshot
    );
    assert_eq!(
        f.installer
            .install(f.artifacts(2))
            .await
            .unwrap()
            .changed_files,
        4
    );
    assert_eq!(
        f.json(".claude.json")["mcpServers"]["agentchat"]["command"],
        "/test-owned/bin/agentchat-2"
    );
    assert_eq!(
        f.json(".claude/settings.json")["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for name in ["ownership.json", "last-backup.json", "install.lock"] {
        assert_eq!(
            fs::metadata(f.installer.state_directory().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(!f.installer.state_directory().join("pending.json").exists());
}

#[tokio::test]
async fn all_destinations_validate_before_any_host_file_changes() {
    for bad in [
        "{",
        r#"{"hooks":[],"hooks":{}}"#,
        r#"{"hooks":{"SessionStart":{}}}"#,
        r#"{"nested":{"$serde_json::private::Number":"123"}}"#,
        r#"{"nested":{"\u0024serde_json::private::RawValue":"null"}}"#,
    ] {
        let f = Fixture::new();
        f.seed();
        let codex = fs::read(f.config.join(".codex/config.toml")).unwrap();
        let registry = fs::read(f.config.join(".claude.json")).unwrap();
        fs::write(f.config.join(".claude/settings.json"), bad).unwrap();
        assert_eq!(
            f.installer.install(f.artifacts(1)).await.unwrap_err().code,
            ErrorCode::ConfigurationConflict
        );
        assert_eq!(
            fs::read(f.config.join(".codex/config.toml")).unwrap(),
            codex
        );
        assert_eq!(fs::read(f.config.join(".claude.json")).unwrap(), registry);
        assert!(!f.config.join(".agents").exists());
        assert!(!f.installer.state_directory().join("pending.json").exists());
    }
}

#[tokio::test]
async fn unowned_identical_names_and_preexisting_skill_directories_are_collisions() {
    for skill in [false, true] {
        let f = Fixture::new();
        f.seed();
        if skill {
            fs::create_dir_all(f.config.join(".agents/skills/agentchat")).unwrap();
            fs::write(
                f.config.join(".agents/skills/agentchat/foreign.txt"),
                "preserve",
            )
            .unwrap();
        } else {
            let mut value = f.json(".claude.json");
            value["mcpServers"]["agentchat"] =
                json!({"type":"stdio","command":"/test-owned/bin/agentchat-1","args":["mcp"]});
            fs::write(
                f.config.join(".claude.json"),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
        }
        let before = fs::read(f.config.join(".claude.json")).unwrap();
        assert_eq!(
            f.installer.install(f.artifacts(1)).await.unwrap_err().code,
            ErrorCode::ConfigurationConflict
        );
        assert_eq!(fs::read(f.config.join(".claude.json")).unwrap(), before);
    }
}

#[tokio::test]
async fn foreign_edits_survive_and_changed_or_missing_owned_values_refuse_update() {
    let f = Fixture::new();
    f.seed();
    f.installer.install(f.artifacts(1)).await.unwrap();
    let mut value = f.json(".claude.json");
    value["addedLater"] = json!({"nested":"foreign"});
    fs::write(
        f.config.join(".claude.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    f.installer.install(f.artifacts(2)).await.unwrap();
    assert_eq!(
        f.json(".claude.json")["addedLater"],
        json!({"nested":"foreign"})
    );
    for delete in [false, true] {
        let mut value = f.json(".claude.json");
        if delete {
            value["mcpServers"]
                .as_object_mut()
                .unwrap()
                .remove("agentchat");
        } else {
            value["mcpServers"]["agentchat"]["enabled"] = json!(false);
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(f.config.join(".claude.json"), &bytes).unwrap();
        assert_eq!(
            f.installer.install(f.artifacts(3)).await.unwrap_err().code,
            ErrorCode::ConfigurationConflict
        );
        assert_eq!(fs::read(f.config.join(".claude.json")).unwrap(), bytes);
    }
}

#[tokio::test]
async fn concurrent_installations_have_one_owner_and_no_duplicate_hook_members() {
    let f = Fixture::new();
    f.seed();
    let mut jobs = Vec::new();
    for _ in 0..8 {
        let installer = f.installer.clone();
        let artifacts = f.artifacts(1);
        jobs.push(tokio::spawn(async move {
            installer.install(artifacts).await.unwrap()
        }));
    }
    let mut changed = 0;
    for job in jobs {
        changed += job.await.unwrap().changed_files;
    }
    assert_eq!(changed, 4);
    assert_eq!(
        f.json(".claude/settings.json")["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn symlinks_foreign_types_and_outside_roots_are_never_replaced() {
    for parent in [false, true] {
        let f = Fixture::new();
        f.seed();
        let foreign = f.root.path().join("foreign");
        fs::write(&foreign, "retain").unwrap();
        if parent {
            let directory = f.config.join(".agents");
            symlink(f.root.path(), directory).unwrap();
        } else {
            fs::remove_file(f.config.join(".claude.json")).unwrap();
            symlink(&foreign, f.config.join(".claude.json")).unwrap();
        }
        assert_eq!(
            f.installer.install(f.artifacts(1)).await.unwrap_err().code,
            ErrorCode::ConfigurationConflict
        );
        assert_eq!(fs::read_to_string(foreign).unwrap(), "retain");
    }
    let f = Fixture::new();
    let mut artifacts = f.artifacts(1);
    artifacts[0].destination = f.root.path().join("outside.toml");
    assert_eq!(
        f.installer.install(artifacts).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert!(!f.root.path().join("outside.toml").exists());
}

#[tokio::test]
async fn malformed_toml_unsupported_ownership_and_oversized_plans_refuse_mutation() {
    let f = Fixture::new();
    f.seed();
    fs::write(f.config.join(".codex/config.toml"), "model = [").unwrap();
    assert_eq!(
        f.installer.install(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    fs::write(f.config.join(".codex/config.toml"), "").unwrap();
    let mut artifacts = f.artifacts(1);
    artifacts[3].content = Content::File {
        text: "x".repeat(1024 * 1024 + 1),
        exclusive_directory: true,
    };
    assert_eq!(
        f.installer.install(artifacts).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert!(!f.config.join(".agents").exists());
    f.installer.install(f.artifacts(1)).await.unwrap();
    let path = f.installer.state_directory().join("ownership.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["version"] = json!(2);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        f.installer.install(f.artifacts(2)).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
}

struct StopAt(InstallStage);
impl InstallProgress for StopAt {
    fn reached(&self, stage: InstallStage) -> agentchat_core::Result<()> {
        if self.0 == stage {
            Err(agentchat_core::Error::new(
                ErrorCode::IoError,
                "Synthetic interruption.",
            ))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn recovery_refuses_intervening_foreign_edits_and_preserves_pending_backup() {
    let f = Fixture::new();
    f.seed();
    let stopped = f
        .installer
        .clone()
        .with_progress(Arc::new(StopAt(InstallStage::Published(0))));
    assert_eq!(
        stopped.install(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::IoError
    );
    let foreign = "{\"foreignAfterCrash\":true}";
    fs::write(f.config.join(".claude.json"), foreign).unwrap();
    assert_eq!(
        f.installer.install(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(
        fs::read_to_string(f.config.join(".claude.json")).unwrap(),
        foreign
    );
    assert!(f.installer.state_directory().join("pending.json").exists());
    assert!(
        !f.installer
            .state_directory()
            .join("ownership.json")
            .exists()
    );
}

#[tokio::test]
async fn overlapping_and_duplicate_selectors_cannot_claim_foreign_or_shared_values() {
    let f = Fixture::new();
    f.seed();
    let mut artifacts = f.artifacts(1);
    artifacts.push(Artifact {
        key: "overlap".into(),
        destination: f.config.join(".claude.json"),
        content: Content::Json {
            path: vec!["mcpServers".into()],
            value: json!({}),
        },
    });
    assert_eq!(
        f.installer.install(artifacts).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    let mut artifacts = f.artifacts(1);
    let mut duplicate = artifacts[2].clone();
    duplicate.key = "duplicate".into();
    artifacts.push(duplicate);
    assert_eq!(
        f.installer.install(artifacts).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    let other = Installer::new(
        f.root.path().join("other-agentchat"),
        vec![f.config.clone()],
    )
    .unwrap();
    f.installer.install(f.artifacts(1)).await.unwrap();
    assert_eq!(
        other.install(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
}

#[tokio::test]
async fn inline_toml_parents_and_separately_owned_hook_groups_preserve_foreign_values() {
    let f = Fixture::new();
    f.seed();
    fs::write(
        f.config.join(".codex/config.toml"),
        "# inline root\nmcp_servers = { foreign = { command = 'keep' } } # retain\n",
    )
    .unwrap();
    let mut artifacts = f.artifacts(1);
    let mut second = artifacts[2].clone();
    second.key = "claude.another-hook".into();
    second.content = Content::JsonArray {
        path: vec!["hooks".into(), "SessionStart".into()],
        value: json!({"matcher":"resume","hooks":[{"type":"command","command":"another-owned-hook"}]}),
    };
    artifacts.push(second);
    f.installer.install(artifacts.clone()).await.unwrap();
    let text = fs::read_to_string(f.config.join(".codex/config.toml")).unwrap();
    assert!(text.starts_with("# inline root\n"));
    assert!(text.contains("command = 'keep'"));
    assert!(text.contains("# retain"));
    assert_eq!(
        f.json(".claude/settings.json")["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        f.installer.install(artifacts).await.unwrap().changed_files,
        0
    );
}

struct ConcurrentEditor(std::path::PathBuf);
impl InstallProgress for ConcurrentEditor {
    fn reached(&self, stage: InstallStage) -> agentchat_core::Result<()> {
        // Fault injection models an external writer between file publications.
        if stage == InstallStage::Published(0) {
            fs::write(
                &self.0,
                "# external concurrent edit\nmodel = 'keep-new-model'\n",
            )
            .unwrap();
        }
        Ok(())
    }
}
#[tokio::test]
async fn concurrent_external_edit_is_checked_again_before_each_publication() {
    let f = Fixture::new();
    f.seed();
    let path = f.config.join(".codex/config.toml");
    let installer = f
        .installer
        .clone()
        .with_progress(Arc::new(ConcurrentEditor(path.clone())));
    assert_eq!(
        installer.install(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "# external concurrent edit\nmodel = 'keep-new-model'\n"
    );
    assert!(f.installer.state_directory().join("pending.json").exists());
}

#[tokio::test]
async fn journal_owned_partial_staging_file_is_removed_on_recovery_only() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    f.seed();
    let stopped = f
        .installer
        .clone()
        .with_progress(Arc::new(StopAt(InstallStage::Journaled)));
    assert_eq!(
        stopped.install(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::IoError
    );
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(f.installer.state_directory().join("pending.json")).unwrap(),
    )
    .unwrap();
    let bytes = journal["writes"][0]["file"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u8)
        .collect();
    let target = std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes));
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let stage = target.parent().unwrap().join(format!(
        ".agentchat-install-{}-0",
        journal["id"].as_str().unwrap()
    ));
    fs::write(&stage, "incomplete staging bytes").unwrap();
    let foreign = target
        .parent()
        .unwrap()
        .join(".agentchat-install-unowned-foreign");
    fs::write(&foreign, "retain foreign bytes").unwrap();
    let result = f.installer.install(f.artifacts(1)).await.unwrap();
    assert!(result.recovered);
    assert_eq!(result.changed_files, 0);
    assert!(!stage.exists());
    assert_eq!(fs::read_to_string(foreign).unwrap(), "retain foreign bytes");
}
