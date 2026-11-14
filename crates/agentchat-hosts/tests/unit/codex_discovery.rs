//! Daemon discovery selects the matching actual image rather than the PATH shim.
use super::*;
use serde_json::{Value, json};
use std::os::unix::net::UnixListener;

fn probe() -> Value {
    json!({
        "status": "running",
        "managedCodexPath": std::env::current_exe().unwrap(),
        "managedCodexVersion": "0.163.0",
        "socketPath": "/tmp/existing-codex.sock",
        "cliVersion": "0.162.1",
        "appServerVersion": "0.163.0"
    })
}

#[test]
fn older_inspection_shim_does_not_reject_matching_managed_native_and_server() {
    let daemon = Daemon::decode(&serde_json::to_vec(&probe()).unwrap()).unwrap();
    assert_eq!(daemon.version, "0.163.0");
    assert_eq!(
        daemon.executable,
        std::env::current_exe().unwrap().canonicalize().unwrap()
    );
    // Parsing is only location selection. inspect() still checks this actual
    // executable's --version, and launch separately requires profile admission.
}

#[test]
fn matching_shim_cannot_mask_managed_server_mismatch_or_invalid_location() {
    for change in 0..4 {
        let mut value = probe();
        value["cliVersion"] = json!("0.163.0");
        match change {
            0 => value["managedCodexVersion"] = json!("0.162.1"),
            1 => value["status"] = json!("stopped"),
            2 => value["managedCodexPath"] = json!("relative/codex"),
            _ => value["socketPath"] = json!("relative/socket"),
        }
        assert_eq!(
            Daemon::decode(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedHost
        );
    }
}

#[test]
fn reported_socket_symlink_resolves_before_strict_proxy_validation() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("server.sock");
    let _listener = UnixListener::bind(&target).unwrap();
    let reported = root.path().join("current.sock");
    std::os::unix::fs::symlink(&target, &reported).unwrap();

    assert_eq!(
        canonical_socket(&reported).unwrap(),
        target.canonicalize().unwrap()
    );
    assert_eq!(
        canonical_socket(&root.path().join("missing.sock"))
            .unwrap_err()
            .code,
        ErrorCode::RuntimeUnavailable
    );
}
