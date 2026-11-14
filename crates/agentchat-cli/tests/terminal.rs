//! Offline entry points must not discover projects, run hosts or mutate state.
mod support;
use std::{fs, time::Duration};
use support::Fixture;

#[tokio::test]
async fn root_help_and_version_are_offline_with_invalid_state_and_context() {
    let f = Fixture::new();
    let state = f.root.path().join("foreign-state-file");
    fs::write(&state, b"preserve foreign state").unwrap();
    let configuration = f.config.join("foreign-settings.json");
    fs::write(&configuration, b"{\"preserve\":true}\n").unwrap();
    let banner = include_str!("../assets/banner.txt");
    for args in [
        vec![],
        vec!["--help"],
        vec!["-h"],
        vec!["--version"],
        vec!["-V"],
        vec!["--json", "--version"],
        vec!["--json", "--help"],
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            f.command(&args)
                .env("AGENTCHAT_HOME", &state)
                .env("AGENTCHAT_CONTEXT", "invalid-context")
                .env("PATH", "")
                .env("NO_COLOR", "1")
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        let version = args.contains(&"--version") || args.contains(&"-V");
        if version && args.contains(&"--json") {
            let identity: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(identity["version"], env!("CARGO_PKG_VERSION"));
            assert_eq!(identity["command"], "agentchat");
            assert_eq!(identity["commit"].as_str().unwrap().len(), 40);
        } else {
            assert!(text.starts_with(banner), "{args:?}: {text}");
            assert!(text.contains("craftguild/agentchat.rs"));
            if version {
                assert!(
                    text.lines()
                        .any(|line| line == format!("agentchat {}", env!("CARGO_PKG_VERSION")))
                );
            } else {
                assert!(text.contains("Usage: agentchat"));
                assert!(text.contains("Commands:"));
            }
        }
        assert_eq!(fs::read(&state).unwrap(), b"preserve foreign state");
        assert_eq!(fs::read(&configuration).unwrap(), b"{\"preserve\":true}\n");
        assert!(!f.workspace.join(".agentchat").exists());
        assert!(!f.manager.home().exists());
    }
    // An explicit machine-mode invocation lacking an operation remains a
    // structured argument error, rather than accidentally becoming banner text.
    let output = f.raw(&["--json"], None).await;
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "invalid_argument");
    assert!(output.stderr.is_empty());
    assert!(!f.manager.home().exists());
}
