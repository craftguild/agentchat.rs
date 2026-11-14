use super::observation;
use agentchat_core::model::HostKind;

#[test]
fn version_observation_is_diagnostic_and_api_checks_are_deferred() {
    let value = observation(HostKind::Codex, Some("codex-cli 0.163.0".into()));
    assert_eq!(value["state"], "candidate");
    assert_eq!(value["api_surface"], "checked_on_use");
    assert_eq!(value["best_effort"], true);
    assert_eq!(value["version"], "codex-cli 0.163.0");
}

#[test]
fn another_well_formed_version_is_also_a_candidate() {
    for (host, version) in [
        (HostKind::Codex, "codex-cli 0.163.1"),
        (HostKind::Claude, "2.2.0 (Claude Code)"),
    ] {
        let value = observation(host, Some(version.into()));
        assert_eq!(value["state"], "candidate");
        assert_eq!(value["best_effort"], true);
    }
}
