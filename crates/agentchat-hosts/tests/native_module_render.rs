//! Rendering never admits a profile or changes the host's execution environment.
use agentchat_core::ErrorCode;
use agentchat_hosts::claude::{module::render, observer::NativeProfile};
use std::{collections::BTreeMap, path::Path};
#[test]
fn invalid_identity_labels_and_host_home_overrides_refuse_without_host_execution() {
    let env = BTreeMap::from([("AGENTCHAT_HOME".into(), "/private/state".into())]);
    for version in ["", "line\nbreak"] {
        let profile = NativeProfile {
            executable: "/native/claude".into(),
            version: version.into(),
        };
        assert_eq!(
            render(Path::new("/app/agentchat"), &profile, &env)
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedHost
        );
    }
    let profile = NativeProfile {
        executable: "/native/claude".into(),
        version: "next-canary".into(),
    };
    for name in ["HOME", "CODEX_HOME", "PATH"] {
        let mut invalid = env.clone();
        invalid.insert(name.into(), "/override".into());
        assert_eq!(
            render(Path::new("/app/agentchat"), &profile, &invalid)
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    let mut invalid = env;
    invalid.insert("AGENTCHAT_HOME".into(), "/state/../foreign".into());
    assert!(render(Path::new("/app/agentchat"), &profile, &invalid).is_err());
}

#[test]
fn observed_candidate_renders_the_current_companion_and_pins_its_label() {
    let env = BTreeMap::from([("AGENTCHAT_HOME".into(), "/isolated/state".into())]);
    let source = render(
        Path::new("/isolated/agentchat"),
        &NativeProfile {
            executable: "/isolated/claude".into(),
            version: "next-canary".into(),
        },
        &env,
    )
    .unwrap();
    assert!(source.contains("import { createInputs } from './input.js'"));
    assert!(source.contains("VERSION = \"next-canary\""));
    assert!(source.contains("on('session.append'"));
    assert!(!source.contains("$.prompt.submit"));
    assert!(!agentchat_hosts::claude::module::input_source().contains("$.prompt.submit"));
}

#[test]
fn rendered_module_classifies_literal_required_api_failures() {
    let source = render(
        Path::new("/isolated/agentchat"),
        &NativeProfile {
            executable: "/isolated/claude".into(),
            version: "future-build".into(),
        },
        &BTreeMap::from([("AGENTCHAT_HOME".into(), "/isolated/state".into())]),
    )
    .unwrap();
    for pointer in [
        "session.id",
        "session.append",
        "command.register",
        "process.run",
        "process.spawn",
        "env.get",
    ] {
        assert!(
            source.contains(pointer),
            "missing capability check for {pointer}"
        );
    }
    assert!(source.contains("AgentChat native capability unavailable:"));
    assert!(source.contains("function capabilityFailure(message)"));
    assert!(source.contains("const id = await $.session.id()"));
    assert!(source.contains("capabilityError !== null ? { drop: capabilityError }"));
    assert!(!source.contains("checkCapabilities"));
    assert!(!source.contains("$?."));
}

#[test]
fn exact_303_candidate_pins_its_own_engine_without_admitting_the_profile() {
    let before = agentchat_hosts::capabilities();
    let source = render(
        Path::new("/isolated/agentchat"),
        &NativeProfile {
            executable: "/isolated/claude".into(),
            version: "2.1.303".into(),
        },
        &BTreeMap::from([("AGENTCHAT_HOME".into(), "/isolated/state".into())]),
    )
    .unwrap();
    assert!(source.contains("VERSION = \"2.1.303\""));
    assert!(source.contains("import { createInputs } from './input.js'"));
    assert!(agentchat_hosts::claude::module::has_startup_gate_source(
        "2.1.303"
    ));
    assert_eq!(agentchat_hosts::capabilities(), before);
}

#[test]
#[ignore = "requires explicit AGENTCHAT_NATIVE_VALIDATOR pointing to native Claude 2.1.302; no session or sign-in"]
fn actual_302_static_analyzer_sees_all_hooks_and_same_file_sdk_calls() {
    actual_static_analyzer("2.1.302");
}

#[test]
#[ignore = "requires explicit AGENTCHAT_NATIVE_VALIDATOR pointing to native Claude 2.1.303; no session or sign-in"]
fn actual_303_static_analyzer_sees_all_hooks_and_same_file_sdk_calls() {
    actual_static_analyzer("2.1.303");
}

fn actual_static_analyzer(expected: &str) {
    let native = std::path::PathBuf::from(
        std::env::var_os("AGENTCHAT_NATIVE_VALIDATOR").expect("explicit exact native validator"),
    )
    .canonicalize()
    .unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let state = root.join("state");
    let config = root.join("config");
    std::fs::create_dir(&state).unwrap();
    std::fs::create_dir(&config).unwrap();
    let command = || {
        let mut command = std::process::Command::new(&native);
        command
            .env("AGENTCHAT_HOME", &state)
            .env("AGENTCHAT_USER_CONFIG_ROOT", &config)
            .env("CLAUDE_CONFIG_DIR", &config)
            .env("DISABLE_AUTOUPDATER", "1");
        command
    };
    let version = command().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(
        String::from_utf8(version.stdout)
            .unwrap()
            .starts_with(&format!("{expected} "))
    );
    let plugin = root.join("plugin");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::create_dir(plugin.join("hooks")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"agentchat-native-static-test","version":"0.1.0"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join("hooks/hooks.json"),
        r#"{"modules":["./register.js"]}"#,
    )
    .unwrap();
    let source = render(
        &root.join("agentchat"),
        &NativeProfile {
            executable: native.clone(),
            version: expected.into(),
        },
        &BTreeMap::from([("AGENTCHAT_HOME".into(), state.to_str().unwrap().into())]),
    )
    .unwrap();
    std::fs::write(plugin.join("hooks/register.js"), source).unwrap();
    std::fs::write(
        plugin.join("hooks/input.js"),
        agentchat_hosts::claude::module::input_source(),
    )
    .unwrap();
    let result = command()
        .args(["plugin", "validate"])
        .arg(&plugin)
        .arg("--json")
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(result.status.success(), "{report}");
    assert_eq!(report["success"], true);
    let notes = report["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| v["notes"].as_array().into_iter().flatten())
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for required in [
        "classic.FileChanged",
        "classic.Stop",
        "session.append",
        "$.command.list (via marker)",
        "$.session.append (via inputIo)",
        "$.session.cwd (via facts, inputIo, stream)",
        "$.session.id",
        "$.process.run (via inputIo",
    ] {
        assert!(notes.contains(required), "missing {required}: {notes}");
    }
    // The real parser also checks result shapes, which Node SDK simulations
    // and static call analysis cannot prove. This invokes the native test kit
    // with only event stubs; it starts no session, model, tool or sign-in flow.
    std::fs::write(
        plugin.join("input-block.test.ts"),
        include_str!("native_input_block.test.ts")
            .replace("version:'2.1.302'", &format!("version:'{expected}'")),
    )
    .unwrap();
    let tested = command()
        .current_dir(&plugin)
        .args(["plugin", "test"])
        .output()
        .unwrap();
    assert!(
        tested.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&tested.stdout),
        String::from_utf8_lossy(&tested.stderr)
    );
}
