//! Explicit SDK simulation: no native host, profile, model or admission.
#[test]
fn hybrid_sdk_preserves_acceptance_fifo_and_native_queue_continuations() {
    let root = tempfile::tempdir().unwrap();
    let module = root.path().join("input.mjs");
    std::fs::write(&module, agentchat_hosts::claude::module::input_source()).unwrap();
    let script = root.path().join("fixture.mjs");
    std::fs::write(&script, include_str!("native_module/input_fixture.mjs")).unwrap();
    let output = std::process::Command::new("node")
        .arg(&script)
        .arg(&module)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["actual_native_proof"], false);
    assert_eq!(report["cases"].as_array().unwrap().len(), 20);
}
