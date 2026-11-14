//! Actual distribution integration: early CLI output must remain independent of state.

use std::{fs, path::Path, process::Command};

#[test]
#[ignore = "requires the verified built distribution; run make acceptance"]
fn standalone_presentation_and_sbom_need_no_project_or_tools() {
    let binary = std::env::var_os("AGENTCHAT_STANDALONE_BINARY").expect("verified binary");
    let sbom = std::env::var_os("AGENTCHAT_STANDALONE_SBOM").expect("verified SBOM");
    let expected_sbom = fs::read(sbom).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let copy = root.join("agentchat");
    fs::copy(Path::new(&binary), &copy).unwrap();
    fs::write(root.join("Cargo.toml"), "invalid unrelated manifest").unwrap();
    fs::write(
        root.join("craftguild.toml"),
        "invalid unrelated configuration",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(&copy)
            .args(args)
            .current_dir(root)
            .env("PATH", "")
            .env("AGENTCHAT_HOME", root.join("state"))
            .env("AGENTCHAT_USER_CONFIG_ROOT", root.join("config"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        output.stdout
    };
    let help = run(&["--help"]);
    assert_eq!(run(&[]), help);
    assert!(
        String::from_utf8(help)
            .unwrap()
            .contains("craftguild/agentchat.rs")
    );
    let version = String::from_utf8(run(&["--version"])).unwrap();
    assert!(version.contains(&format!("agentchat {}", env!("CARGO_PKG_VERSION"))));
    assert!(version.contains(&std::env::var("AGENTCHAT_STANDALONE_REVISION").unwrap()));
    let identity: serde_json::Value =
        serde_json::from_slice(&run(&["--version", "--json"])).unwrap();
    assert_eq!(
        identity["commit"],
        std::env::var("AGENTCHAT_STANDALONE_REVISION").unwrap()
    );
    assert_eq!(run(&["sbom"]), expected_sbom);
    assert!(!root.join("state").exists());
    assert!(!root.join("config").exists());
}
