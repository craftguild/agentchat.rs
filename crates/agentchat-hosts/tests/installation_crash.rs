//! Hard process exits prove recovery without relying on Drop cleanup.
mod support;
use agentchat_hosts::install::{InstallProgress, InstallStage, Installer};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use support::Fixture;

struct Cut(String);
impl InstallProgress for Cut {
    fn reached(&self, stage: InstallStage) -> agentchat_core::Result<()> {
        let name = match stage {
            InstallStage::Validated => "validated".into(),
            InstallStage::Journaled => "journal".into(),
            InstallStage::Published(i) => format!("file-{i}"),
            InstallStage::OwnershipPublished => "ownership".into(),
            InstallStage::Complete => "complete".into(),
        };
        if name == self.0 {
            std::process::exit(86);
        }
        Ok(())
    }
}
#[tokio::test]
#[ignore = "subprocess helper; passing parent executes every installation cut"]
async fn installation_child() {
    let state = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_HOME").unwrap());
    let config = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_USER_CONFIG_ROOT").unwrap());
    let installer = Installer::new(state, vec![config.clone()])
        .unwrap()
        .with_progress(Arc::new(Cut(
            std::env::var("AGENTCHAT_TEST_INSTALL_CUT").unwrap()
        )));
    installer
        .install(support::artifacts(&config, 1))
        .await
        .unwrap();
    panic!("Expected a hard process exit at the selected installation cut.");
}
fn no_staging(path: &Path) {
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        assert!(
            !name.to_string_lossy().starts_with(".agentchat-install-")
                && !name.to_string_lossy().starts_with(".agentchat-state-")
        );
        if entry.file_type().unwrap().is_dir() {
            no_staging(&entry.path());
        }
    }
}
#[tokio::test]
async fn eight_process_cuts_restore_one_owned_installation_before_applying_an_update() {
    for point in [
        "validated",
        "journal",
        "file-0",
        "file-1",
        "file-2",
        "file-3",
        "ownership",
        "complete",
    ] {
        let f = Fixture::new();
        f.seed();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "installation_child"])
            .env("AGENTCHAT_HOME", &f.state)
            .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
            .env("AGENTCHAT_TEST_INSTALL_CUT", point)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Installation child did not reach {point}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(86), "{point}");
        let report = f.installer.install(f.artifacts(2)).await.unwrap();
        assert_eq!(
            report.recovered,
            !matches!(point, "validated" | "complete"),
            "{point}"
        );
        assert_eq!(report.changed_files, 4);
        assert_eq!(
            f.json(".claude.json")["mcpServers"]["agentchat"]["command"],
            "/test-owned/bin/agentchat-2"
        );
        assert_eq!(
            f.json(".claude.json")["oauthAccount"]["redacted"],
            "test-only"
        );
        assert_eq!(
            f.json(".claude/settings.json")["permissions"]["defaultMode"],
            "plan"
        );
        assert_eq!(
            f.json(".claude/settings.json")["hooks"]["SessionStart"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            f.installer
                .install(f.artifacts(2))
                .await
                .unwrap()
                .changed_files,
            0
        );
        assert!(!f.installer.state_directory().join("pending.json").exists());
        let backup: serde_json::Value = serde_json::from_slice(
            &fs::read(f.installer.state_directory().join("last-backup.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(backup["writes"].as_array().unwrap().len(), 4);
        no_staging(f.root.path());
    }
}
