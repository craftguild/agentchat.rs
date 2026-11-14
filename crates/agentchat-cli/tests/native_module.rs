//! Real Rust helper/MCP integration with explicitly simulated native SDK getters.
#[path = "native_module/input_runtime.rs"]
mod input_runtime;
#[path = "native_module/runtime.rs"]
mod runtime;
mod support;
use agentchat_hosts::claude::{module::render, observer::NativeProfile};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use support::Fixture;

// Only fresh-image copying/offline startup is serialized. Each SDK scenario
// retains its own runtime and executes concurrently after this fixture preflight.
static EXECUTABLE_PREFLIGHT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn rendered_native_module_binds_bootstrap_promotes_and_isolates_clear_with_sdk_simulation() {
    simulate("lifecycle").await;
}
#[tokio::test]
async fn rendered_302_bundle_binds_real_helpers_and_mcp_without_inheriting_301_version() {
    simulate("lifecycle302").await;
}
#[tokio::test]
async fn rendered_302_lifecycle_mismatch_reports_fixed_stage_without_granting_readiness() {
    simulate("fault_stage302").await;
}
#[tokio::test]
async fn missing_native_api_is_reported_before_participation_commit() {
    simulate("capability302").await;
}
#[tokio::test]
async fn used_background_native_api_failure_blocks_the_affected_prompt() {
    simulate("capability_spawn302").await;
}
#[tokio::test]
async fn used_prompt_native_api_failure_is_named_on_the_same_prompt() {
    simulate("capability_env302").await;
}
#[tokio::test]
async fn swallowed_input_callback_failure_is_named_on_the_next_prompt() {
    simulate("capability_input_cwd302").await;
}
#[tokio::test]
async fn rendered_302_resume_waits_for_live_identity_and_preserves_veto_origin_and_abort() {
    for case in [
        "resume_order302",
        "resume_veto302",
        "resume_foreign302",
        "resume_abort302",
    ] {
        simulate(case).await;
    }
}
#[tokio::test]
async fn rendered_302_initial_start_requires_one_permitted_native_lifecycle_and_retains_later_pause()
 {
    for case in [
        "startup302",
        "early_startup302",
        "early_startup_block302",
        "early_startup_stop302",
        "early_startup_foreign302",
        "early_startup_abort302",
        "early_startup_late_abort302",
    ] {
        simulate(case).await;
    }
}
#[tokio::test]
async fn real_helpers_runtime_and_sqlite_keep_hybrid_full_input_fifo_with_sdk_simulation() {
    simulate("inputs302").await;
}

#[tokio::test]
async fn prompt_before_queue_result_keeps_original_head_and_waits_for_actual_acceptance() {
    simulate("inputs_prompt_first302").await;
}

#[tokio::test]
async fn kept_append_with_lost_result_keeps_original_fifo_and_no_retry() {
    simulate("inputs_append_unknown302").await;
}
#[tokio::test]
async fn kept_native_queue_with_lost_result_keeps_original_fifo_and_no_retry() {
    simulate("inputs_queue_unknown302").await;
}
#[tokio::test]
async fn exact_303_helpers_and_runtime_keep_hybrid_full_input_with_sdk_simulation() {
    simulate("inputs303").await;
}
#[tokio::test]
async fn rendered_native_controls_keep_readiness_separate_from_pause_and_approval() {
    simulate("controls").await;
}
#[tokio::test]
async fn rendered_native_module_recovers_owned_stream_loss_without_model_input_or_pause_release() {
    simulate("recovery").await;
}
#[tokio::test]
async fn rendered_302_context_refresh_recaptures_scope_without_releasing_pause_or_restarting_mcp() {
    simulate("refresh302").await;
}
#[tokio::test]
async fn rendered_native_call_retries_exhausted_stream_without_releasing_pause() {
    simulate("stream_exhaustion").await;
}
#[tokio::test]
async fn rendered_native_early_classic_start_waits_for_the_bound_session_without_poisoning_readiness()
 {
    simulate("early_startup").await;
}
#[tokio::test]
async fn rendered_native_overlapping_classic_start_keeps_the_exact_resume_receipt() {
    simulate("early_overlap").await;
}
#[tokio::test]
async fn rendered_native_early_resume_keeps_veto_origin_abort_and_interruption_controls() {
    for case in [
        "early_resume",
        "early_block",
        "early_stop",
        "early_foreign",
        "early_abort",
        "early_late_abort",
    ] {
        simulate(case).await;
    }
}
#[tokio::test]
async fn rendered_native_cwd_change_rejects_the_old_attachment_without_cached_presence() {
    simulate("cwd_fence").await;
}
async fn simulate(scenario: &str) {
    let f = Fixture::new();
    let version = if scenario.ends_with("303") {
        "2.1.303"
    } else if scenario.ends_with("302") {
        "2.1.302"
    } else {
        "2.1.301"
    };
    let output = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .expect("pinned Node.js is required for native module tests");
    assert!(output.status.success());
    let node = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let preflight = EXECUTABLE_PREFLIGHT.lock().await;
    let binary = f
        .root
        .path()
        .join("agentchat '__AGENTCHAT_NATIVE_EXECUTABLE__'");
    std::fs::copy(env!("CARGO_BIN_EXE_agentchat"), &binary).unwrap();
    // Fresh executable copies may incur serialized OS startup validation before
    // main(), even for offline version output. Complete this fixture preflight
    // before exercising the unchanged native helper/SDK deadlines. Recorded Mac
    // copied-image probes exceed 100 seconds; Linux retains its 15-second bound.
    let preflight_seconds = if cfg!(target_os = "macos") { 180 } else { 15 };
    let warm = tokio::time::timeout(Duration::from_secs(preflight_seconds), async {
        loop {
            match tokio::process::Command::new(&binary)
                .arg("--version")
                .env("AGENTCHAT_HOME", f.manager.home())
                .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
            {
                // Linux can briefly refuse this freshly copied fixture
                // executable before main(). Retry only that kernel refusal
                // within the existing fixture preflight deadline.
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                outcome => break outcome,
            }
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert!(warm.status.success());
    assert!(warm.stderr.is_empty());
    assert!(!f.manager.home().exists());
    drop(preflight);

    let environment = BTreeMap::from([
        (
            "AGENTCHAT_HOME".into(),
            f.manager.home().to_str().unwrap().into(),
        ),
        (
            "AGENTCHAT_USER_CONFIG_ROOT".into(),
            f.config.to_str().unwrap().into(),
        ),
    ]);
    let source = render(
        &binary,
        &NativeProfile {
            executable: node.clone(),
            version: version.into(),
        },
        &environment,
    )
    .unwrap();
    let module = f.root.path().join("register.mjs");
    std::fs::write(&module, source).unwrap();
    if agentchat_hosts::claude::module::has_startup_gate_source(version) {
        std::fs::write(
            f.root.path().join("input.js"),
            agentchat_hosts::claude::module::input_source(),
        )
        .unwrap();
        std::fs::write(f.root.path().join("package.json"), "{\"type\":\"module\"}").unwrap();
    }
    // Runtime source paths can point at a preceding checkout when a shared
    // Cargo target reuses this test binary. Track the asset at compile time and
    // execute these exact bytes from the isolated fixture instead.
    let script = f.root.path().join("sdk_fixture.mjs");
    std::fs::write(&script, include_bytes!("native_module/sdk_fixture.mjs")).unwrap();
    std::fs::write(
        f.root.path().join("input_integration.mjs"),
        include_bytes!("native_module/input_integration.mjs"),
    )
    .unwrap();
    std::fs::write(
        f.root.path().join("resume_integration.mjs"),
        include_bytes!("native_module/resume_integration.mjs"),
    )
    .unwrap();
    let mut child = tokio::process::Command::new(&node);
    child
        .arg(&script)
        .arg(&module)
        .arg(&binary)
        .arg(f.manager.home())
        .arg(&f.config)
        .arg(&f.workspace)
        .arg(scenario)
        .env("AGENTCHAT_HOME", f.root.path().join("decoy-state"))
        .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("AGENTCHAT_LAUNCH_ROOT")
        .env_remove("AGENTCHAT_LAUNCH_GENERATION")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let authority = if scenario == "controls"
        || scenario.starts_with("inputs")
        || scenario == "capability_input_cwd302"
    {
        // Bootstrap/promotion is exercised by the other seven SDK scenarios.
        // Controls need the explicit synthetic runtime before the first native
        // status/init call could lazily start the production-gated composition.
        f.initialize().await;
        let (stop, receive) = tokio::sync::oneshot::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let manager = f.manager.clone();
        let workspace = f.workspace.clone();
        let full = scenario.starts_with("inputs");
        let expected_version = version.to_owned();
        let task = tokio::spawn(async move {
            if full {
                input_runtime::serve(manager, workspace, expected_version, receive, started).await;
            } else {
                runtime::serve(manager, workspace, receive, started).await;
            }
        });
        ready.await.unwrap();
        Some((stop, task))
    } else {
        None
    };
    let result = tokio::time::timeout(
        Duration::from_secs(35),
        child.spawn().unwrap().wait_with_output(),
    )
    .await
    .unwrap()
    .unwrap();
    let authority = if let Some((stop, authority)) = authority {
        let _ = stop.send(());
        Some(tokio::time::timeout(Duration::from_secs(5), authority).await)
    } else {
        None
    };
    if let Some(project) = f.manager.discover(&f.workspace).await.unwrap() {
        f.stop_lazy(&project).await;
    }
    if !result.status.success()
        || authority
            .as_ref()
            .is_some_and(|result| !matches!(result, Ok(Ok(()))))
    {
        let retained = f.root.path().to_owned();
        std::fs::write(retained.join("failure.stdout"), &result.stdout).unwrap();
        std::fs::write(retained.join("failure.stderr"), &result.stderr).unwrap();
        std::fs::write(retained.join("failure.authority"), format!("{authority:?}")).unwrap();
        let _ = f.root.keep();
        panic!(
            "SDK simulation {scenario} failed; retained {retained:?}, authority {authority:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["fixture_path"].as_str(), script.to_str(), "{report}");
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["actual_native_proof"], false);
    assert!(!f.root.path().join("decoy-state").exists());
}
