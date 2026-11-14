//! Best-effort host admission is based on adapter API surfaces, not versions.

use agentchat_core::{ErrorCode, model::HostKind};

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[test]
fn capabilities_publish_required_surfaces_without_version_profiles() {
    let capabilities = agentchat_hosts::capabilities();
    assert_eq!(capabilities["automatic_wake"], true);
    assert_eq!(capabilities["compatibility"], "best_effort_api_surface");
    assert_eq!(
        capabilities["version_policy"],
        "diagnostic_and_identity_only"
    );
    let surfaces = capabilities["host_surfaces"].as_array().unwrap();
    for (host, required) in [
        (HostKind::Codex, "thread/start"),
        (HostKind::Claude, "session.id"),
    ] {
        let surface = surfaces
            .iter()
            .find(|surface| surface["host"] == serde_json::json!(host))
            .unwrap();
        assert!(
            surface["required_api"]
                .as_array()
                .unwrap()
                .contains(&required.into())
        );
        agentchat_hosts::require_native_launch(host).unwrap();
    }
    assert!(capabilities.get("admitted_profiles").is_none());
    assert!(capabilities.get("required_gate").is_none());
}

#[test]
fn version_is_an_identity_label_instead_of_an_allowlist() {
    let current = platform();
    for host in [HostKind::Codex, HostKind::Claude] {
        for version in ["0.164.0", "future-preview+api1", "2.2.0"] {
            agentchat_hosts::require_native_profile(host, version, &current).unwrap();
        }
        for version in ["", "bad\nversion", "bad\u{7f}version"] {
            assert!(agentchat_hosts::require_native_profile(host, version, &current).is_err());
        }
    }
}

#[test]
fn a_foreign_platform_or_missing_api_is_reported_directly() {
    let foreign = agentchat_hosts::require_native_profile(HostKind::Codex, "0.164.0", "foreign-os")
        .unwrap_err();
    assert_eq!(foreign.code, ErrorCode::UnsupportedHost);
    assert_eq!(foreign.details["os_profile"], "foreign-os");
    assert_eq!(foreign.details["compatibility"], "best_effort_api_surface");

    let missing =
        agentchat_hosts::missing_api(HostKind::Codex, Some("0.164.0"), &["thread/inject_items"]);
    assert_eq!(missing.code, ErrorCode::UnsupportedProtocol);
    assert_eq!(missing.details["host"], "codex");
    assert_eq!(missing.details["host_version"], "0.164.0");
    assert_eq!(
        missing.details["missing_api"],
        serde_json::json!(["thread/inject_items"])
    );
}
