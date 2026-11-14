//! The 302 input companion and hooks use the existing preserving installer.
use agentchat_core::model::HostKind;
use agentchat_hosts::{
    claude::observer::NativeProfile,
    install::{InspectionState, Installer},
    setup::Environment,
};
use serde_json::{Value, json};
#[tokio::test]
async fn full_input_bundle_upgrades_owned_301_without_changing_foreign_policy_or_watches() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let roots = Environment {
        user_config_root: Some(config),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    std::fs::create_dir_all(roots.claude_settings.parent().unwrap()).unwrap();
    let foreign = json!({"permissions":{"defaultMode":"plan","ask":["Write"]},"watchPaths":["foreign"],"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign-stop"}]}],"FileChanged":[{"matcher":"foreign","hooks":[{"type":"command","command":"foreign-watch"}]}]}});
    std::fs::write(&roots.claude_settings, foreign.to_string()).unwrap();
    let home = root.path().join("state");
    let agentchat = root.path().join("agentchat");
    let mut profile = NativeProfile {
        executable: root.path().join("native"),
        version: "2.1.301".into(),
    };
    let installer = Installer::new(&home, roots.tool_roots(&[HostKind::Claude])).unwrap();
    installer
        .install(
            roots
                .claude_native_artifacts(&agentchat, &home, &profile)
                .unwrap(),
        )
        .await
        .unwrap();
    profile.version = "2.1.302".into();
    let artifacts = || {
        roots
            .claude_native_artifacts(&agentchat, &home, &profile)
            .unwrap()
    };
    installer.install(artifacts()).await.unwrap();
    assert_eq!(
        installer.install(artifacts()).await.unwrap().changed_files,
        0
    );
    assert_eq!(
        installer.inspect(artifacts()).await.unwrap().state,
        InspectionState::Healthy
    );
    let settings: Value =
        serde_json::from_slice(&std::fs::read(&roots.claude_settings).unwrap()).unwrap();
    assert_eq!(settings["permissions"], foreign["permissions"]);
    assert_eq!(settings["watchPaths"], foreign["watchPaths"]);
    for event in ["Stop", "FileChanged"] {
        assert_eq!(settings["hooks"][event][0], foreign["hooks"][event][0]);
        assert_eq!(settings["hooks"][event].as_array().unwrap().len(), 2);
        let hook = &settings["hooks"][event][1]["hooks"][0];
        assert_eq!(hook["asyncRewake"], true);
        assert!(
            hook["command"]
                .as_str()
                .unwrap()
                .contains("internal claude-input --mode notice")
        );
    }
    let plugin = roots.claude_skill.parent().unwrap();
    assert_eq!(
        std::fs::read_to_string(plugin.join("hooks/input.js")).unwrap(),
        agentchat_hosts::claude::module::input_source()
    );
    assert!(
        !std::fs::read_to_string(plugin.join("hooks/register.js"))
            .unwrap()
            .contains("$.prompt.submit")
    );
    let registry: Value =
        serde_json::from_slice(&std::fs::read(&roots.claude_registry).unwrap()).unwrap();
    assert_eq!(
        registry["mcpServers"]["agentchat"]["env"]["AGENTCHAT_CLAUDE_NATIVE_VERSION"],
        "2.1.302"
    );
}
