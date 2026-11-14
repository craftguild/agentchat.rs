//! Read-only configuration health uses the installer's actual preservation rules.
mod support;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::install::{
    ComponentState as C, InspectionState as S, InstallProgress, InstallStage,
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    sync::Arc,
    time::Duration,
};
use support::Fixture;

fn snapshot(root: &Path) -> BTreeMap<std::path::PathBuf, (u32, Vec<u8>)> {
    fn visit(root: &Path, at: &Path, map: &mut BTreeMap<std::path::PathBuf, (u32, Vec<u8>)>) {
        let m = fs::symlink_metadata(at).unwrap();
        let bytes = if m.file_type().is_symlink() {
            fs::read_link(at)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else if m.is_file() {
            fs::read(at).unwrap()
        } else {
            Vec::new()
        };
        map.insert(
            at.strip_prefix(root).unwrap().to_owned(),
            (m.permissions().mode(), bytes),
        );
        if m.is_dir() {
            for e in fs::read_dir(at).unwrap() {
                visit(root, &e.unwrap().path(), map);
            }
        }
    }
    let mut map = BTreeMap::new();
    visit(root, root, &mut map);
    map
}

#[tokio::test]
async fn absent_installation_creates_no_state_lock_or_configuration() {
    let f = Fixture::new();
    let before = snapshot(f.root.path());
    assert_eq!(
        f.installer.inspect(f.artifacts(1)).await.unwrap().state,
        S::NotInstalled
    );
    assert_eq!(snapshot(f.root.path()), before);
    fs::create_dir(&f.state).unwrap();
    let before = snapshot(f.root.path());
    assert_eq!(
        f.installer.inspect(f.artifacts(1)).await.unwrap().state,
        S::NotInstalled
    );
    assert_eq!(snapshot(f.root.path()), before);
}

#[tokio::test]
async fn owned_values_and_updates_are_checked_without_disclosing_or_rewriting_foreign_data() {
    let f = Fixture::new();
    f.seed();
    f.installer.install(f.artifacts(1)).await.unwrap();
    let before = snapshot(f.root.path());
    let same = f.installer.inspect(f.artifacts(1)).await.unwrap();
    assert_eq!(same.state, S::Healthy);
    assert_eq!(same.components.len(), 4);
    assert!(same.components.iter().all(|c| c.state == C::Owned));
    let update = f.installer.inspect(f.artifacts(2)).await.unwrap();
    assert_eq!(update.state, S::NeedsUpdate);
    assert!(
        update
            .components
            .iter()
            .all(|c| c.state == C::UpdateAvailable)
    );
    let text = serde_json::to_string(&same).unwrap();
    for secret in [
        "test-only",
        "foreign-model",
        "foreign-tool",
        "oauthAccount",
        "/test-owned/bin",
    ] {
        assert!(!text.contains(secret));
    }
    assert_eq!(snapshot(f.root.path()), before);
    // A foreign sibling change is outside the owned selector's checksum.
    let mut foreign = f.json(".claude.json");
    foreign["oauthAccount"]["extra"] = serde_json::json!("private-sentinel");
    fs::write(
        f.config.join(".claude.json"),
        serde_json::to_vec(&foreign).unwrap(),
    )
    .unwrap();
    let before = snapshot(f.root.path());
    assert_eq!(
        f.installer.inspect(f.artifacts(1)).await.unwrap().state,
        S::Healthy
    );
    assert_eq!(snapshot(f.root.path()), before);
}

#[tokio::test]
async fn missing_changed_and_symbolic_owned_files_are_reported_without_repair() {
    let f = Fixture::new();
    f.seed();
    f.installer.install(f.artifacts(1)).await.unwrap();
    let skill = f.config.join(".agents/skills/agentchat/SKILL.md");
    fs::remove_file(&skill).unwrap();
    let mut registry = f.json(".claude.json");
    registry["mcpServers"]["agentchat"]["command"] = serde_json::json!("externally-changed");
    fs::write(
        f.config.join(".claude.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let before = snapshot(f.root.path());
    let report = f.installer.inspect(f.artifacts(1)).await.unwrap();
    assert_eq!(report.state, S::AttentionRequired);
    assert_eq!(
        report
            .components
            .iter()
            .filter(|c| c.state == C::Conflict)
            .count(),
        2
    );
    assert_eq!(snapshot(f.root.path()), before);
    let target = f.root.path().join("foreign-skill");
    fs::write(&target, "secret-target").unwrap();
    symlink(&target, &skill).unwrap();
    let before = snapshot(f.root.path());
    let report = f.installer.inspect(f.artifacts(1)).await.unwrap();
    assert_eq!(report.state, S::AttentionRequired);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("secret-target")
    );
    assert_eq!(snapshot(f.root.path()), before);
}

struct FailAt(InstallStage);
impl InstallProgress for FailAt {
    fn reached(&self, stage: InstallStage) -> Result<()> {
        if stage == self.0 {
            Err(Error::new(
                ErrorCode::IoError,
                "injected publication failure",
            ))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn pending_publication_is_observed_but_only_explicit_installation_recovers_it() {
    let f = Fixture::new();
    f.seed();
    assert!(
        f.installer
            .clone()
            .with_progress(Arc::new(FailAt(InstallStage::Published(0))))
            .install(f.artifacts(1))
            .await
            .is_err()
    );
    let before = snapshot(f.root.path());
    let report = f.installer.inspect(f.artifacts(1)).await.unwrap();
    assert_eq!(report.state, S::RecoveryRequired);
    assert!(report.components.is_empty());
    assert_eq!(snapshot(f.root.path()), before);
    assert!(f.installer.install(f.artifacts(1)).await.unwrap().recovered);
    assert_eq!(
        f.installer.inspect(f.artifacts(1)).await.unwrap().state,
        S::Healthy
    );
}

#[tokio::test]
async fn an_active_writer_returns_busy_without_waiting_and_unknown_components_are_not_healthy() {
    let f = Fixture::new();
    f.seed();
    f.installer.install(f.artifacts(1)).await.unwrap();
    let lock = fs::File::open(f.installer.state_directory().join("install.lock")).unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let before = snapshot(f.root.path());
    let report = tokio::time::timeout(Duration::from_secs(5), f.installer.inspect(f.artifacts(1)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.state, S::Busy);
    assert!(report.components.is_empty());
    assert_eq!(snapshot(f.root.path()), before);
    drop(lock);
    let report = f.installer.inspect(Vec::new()).await.unwrap();
    assert_eq!(report.state, S::AttentionRequired);
    assert!(report.components.iter().all(|c| c.state == C::Unverified));
}

#[tokio::test]
async fn invalid_ledger_missing_lock_and_oversized_intent_fail_without_creating_metadata() {
    let f = Fixture::new();
    f.seed();
    f.installer.install(f.artifacts(1)).await.unwrap();
    let path = f.installer.state_directory().join("ownership.json");
    let original = fs::read(&path).unwrap();
    fs::write(&path, b"not-json").unwrap();
    let before = snapshot(f.root.path());
    assert_eq!(
        f.installer.inspect(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(snapshot(f.root.path()), before);
    fs::write(&path, original).unwrap();
    let lock = f.installer.state_directory().join("install.lock");
    fs::remove_file(&lock).unwrap();
    let before = snapshot(f.root.path());
    assert_eq!(
        f.installer.inspect(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(snapshot(f.root.path()), before);
    fs::write(lock, b"").unwrap();
    let pending = fs::File::create(f.installer.state_directory().join("pending.json")).unwrap();
    pending.set_len(32 * 1024 * 1024 + 1).unwrap();
    assert_eq!(
        f.installer.inspect(f.artifacts(1)).await.unwrap_err().code,
        ErrorCode::ConfigurationConflict
    );
    assert_eq!(pending.metadata().unwrap().len(), 32 * 1024 * 1024 + 1);
}
