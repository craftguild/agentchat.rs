//! The native group uses the ordinary journal at every publication boundary.
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use agentchat_hosts::{
    claude::observer::NativeProfile,
    install::{InspectionState, InstallProgress, InstallStage, Installer},
    setup::Environment,
};
use serde_json::{Value, json};
use std::{fs, sync::Arc};
struct Fail(InstallStage);
impl InstallProgress for Fail {
    fn reached(&self, stage: InstallStage) -> Result<()> {
        if stage == self.0 {
            Err(Error::new(
                ErrorCode::IoError,
                "Injected native installer interruption.",
            ))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn native_bundle_recovers_each_published_file_without_duplicate_hooks_or_foreign_changes() {
    // Six destinations: MCP, skill, module, manifest, module index and settings.
    let stages = [
        InstallStage::Journaled,
        InstallStage::Published(0),
        InstallStage::Published(1),
        InstallStage::Published(2),
        InstallStage::Published(3),
        InstallStage::Published(4),
        InstallStage::Published(5),
        InstallStage::OwnershipPublished,
    ];
    for stage in stages {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        fs::create_dir(&config).unwrap();
        let roots = Environment {
            user_config_root: Some(config),
            ..Environment::default()
        }
        .resolve()
        .unwrap();
        fs::create_dir_all(roots.claude_settings.parent().unwrap()).unwrap();
        let foreign = json!({"permissions":{"defaultMode":"plan","ask":["Bash"]},"hooks":{"FileChanged":[{"matcher":"foreign","hooks":[{"type":"command","command":"foreign"}]}]}});
        fs::write(
            &roots.claude_settings,
            serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();
        let state = root.path().join("state");
        let profile = NativeProfile {
            executable: root.path().join("native"),
            version: "2.1.301".into(),
        };
        let plan = || {
            roots
                .claude_native_artifacts(&root.path().join("agentchat"), &state, &profile)
                .unwrap()
        };
        let installer = Installer::new(&state, roots.tool_roots(&[HostKind::Claude])).unwrap();
        let error = installer
            .clone()
            .with_progress(Arc::new(Fail(stage)))
            .install(plan())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoError, "{stage:?}");
        assert_eq!(
            installer.inspect(plan()).await.unwrap().state,
            InspectionState::RecoveryRequired,
            "{stage:?}"
        );
        assert!(
            installer.install(plan()).await.unwrap().recovered,
            "{stage:?}"
        );
        assert_eq!(
            installer.install(plan()).await.unwrap().changed_files,
            0,
            "{stage:?}"
        );
        assert_eq!(
            installer.inspect(plan()).await.unwrap().state,
            InspectionState::Healthy,
            "{stage:?}"
        );
        let actual: Value =
            serde_json::from_slice(&fs::read(&roots.claude_settings).unwrap()).unwrap();
        assert_eq!(actual["permissions"], foreign["permissions"], "{stage:?}");
        assert_eq!(
            actual["hooks"]["FileChanged"][0], foreign["hooks"]["FileChanged"][0],
            "{stage:?}"
        );
        assert_eq!(
            actual["hooks"]["FileChanged"].as_array().unwrap().len(),
            2,
            "{stage:?}"
        );
        assert_eq!(
            actual["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "{stage:?}"
        );
    }
}
