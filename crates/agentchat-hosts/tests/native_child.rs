//! Real process supervision, independent of host/root/admission simulations.
use agentchat_core::model::LaunchOwnerState;
use agentchat_hosts::{
    claude::process::BirthState,
    launch::{LaunchOwners, NativeChild},
};
use std::{collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt, time::Duration};

#[test]
#[ignore = "actual managed native-image child; parent invokes this helper"]
fn managed_scope_child() {
    use std::io::IsTerminal;
    let path = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_NATIVE_CHILD_REPORT").unwrap());
    let scope = agentchat_hosts::launch::StartupScope::from_environment()
        .unwrap()
        .unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(
            &serde_json::json!({"root":scope.root,"generation":scope.generation,
            "inherited_context":std::env::var_os("AGENTCHAT_CONTEXT").is_some(),
            "stdin_tty":std::io::stdin().is_terminal(), "stdout_tty":std::io::stdout().is_terminal(),
            "stderr_tty":std::io::stderr().is_terminal(),
            "stdin_device":rustix::termios::ttyname(std::io::stdin(), Vec::new()).ok().map(|name|name.to_string_lossy().into_owned())}),
        )
        .unwrap(),
    )
    .unwrap();
    if std::env::var_os("AGENTCHAT_NATIVE_TTY_MARKER").is_some() {
        println!("AGENTCHAT_NATIVE_STDOUT_MARKER");
        eprintln!("AGENTCHAT_NATIVE_STDERR_MARKER");
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !path.with_extension("stop").exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[tokio::test]
async fn managed_scope_exists_before_the_native_image_can_accept_input() {
    let root = tempfile::tempdir().unwrap();
    let report = root.path().join("managed.json");
    let selected = uuid::Uuid::new_v4();
    let mut child = NativeChild::spawn_with_startup_gate(
        &std::env::current_exe().unwrap(),
        root.path(),
        &[
            "--ignored".into(),
            "--exact".into(),
            "managed_scope_child".into(),
            "--nocapture".into(),
        ],
        &BTreeMap::from([(
            "AGENTCHAT_NATIVE_CHILD_REPORT".into(),
            report.as_os_str().into(),
        )]),
        selected,
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(bytes) = std::fs::read(&report)
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
            {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(result["root"], selected.to_string());
    assert_eq!(
        result["generation"],
        child.identity().generation.to_string()
    );
    assert_eq!(child.state(), LaunchOwnerState::Current);
    std::fs::write(report.with_extension("stop"), b"stop").unwrap();
    assert!(child.wait().await.unwrap().success());
}

#[tokio::test]
async fn actual_native_child_keeps_original_owner_and_supervisor_until_normal_exit() {
    let root = tempfile::tempdir().unwrap();
    let program = std::path::Path::new("/bin/sleep").canonicalize().unwrap();
    let mut child = NativeChild::spawn(
        &program,
        root.path(),
        &[OsString::from("0.2")],
        &BTreeMap::new(),
    )
    .await
    .unwrap();
    assert_eq!(child.owner().executable(), program);
    assert_eq!(child.state(), LaunchOwnerState::Current);
    assert!(LaunchOwners::is_supervisor(child.identity()));
    assert!(child.try_wait().unwrap().is_none());
    let owner = child.owner().clone();
    let identity = child.identity().clone();
    assert!(child.wait().await.unwrap().success());
    assert_eq!(owner.birth_state(), BirthState::Gone);
    assert_eq!(LaunchOwners::observe(&identity), LaunchOwnerState::Gone);
}
#[tokio::test]
async fn cancelled_unbound_startup_terminates_only_its_spawned_native_child() {
    let root = tempfile::tempdir().unwrap();
    let program = std::path::Path::new("/bin/sleep").canonicalize().unwrap();
    let child = NativeChild::spawn(
        &program,
        root.path(),
        &[OsString::from("60")],
        &BTreeMap::new(),
    )
    .await
    .unwrap();
    let owner = child.owner().clone();
    drop(child);
    tokio::time::timeout(Duration::from_secs(5), async {
        while owner.birth_state() != BirthState::Gone {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        agentchat_hosts::claude::process::ProcessIdentity::capture(std::process::id())
            .unwrap()
            .is_current()
    );
}
#[tokio::test]
async fn scripts_and_home_overrides_refuse_before_running_or_editing_any_host_configuration() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("shim");
    let marker = root.path().join("ran");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        NativeChild::spawn(&script, root.path(), &[], &BTreeMap::new())
            .await
            .is_err()
    );
    assert!(!marker.exists());
    for key in ["HOME", "CODEX_HOME", "AGENTCHAT_CONTEXT"] {
        let env = BTreeMap::from([(key.into(), root.path().as_os_str().into())]);
        assert!(
            NativeChild::spawn(
                std::path::Path::new("/bin/sleep"),
                root.path(),
                &["60".into()],
                &env
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn requested_termination_observes_only_the_original_child_and_is_safe_after_exit() {
    use std::os::unix::process::ExitStatusExt;
    let root = tempfile::tempdir().unwrap();
    let mut child = NativeChild::spawn(
        std::path::Path::new("/bin/sleep"),
        root.path(),
        &["60".into()],
        &BTreeMap::new(),
    )
    .await
    .unwrap();
    let owner = child.owner().clone();
    let status = child.terminate().await.unwrap();
    assert_eq!(status.signal(), Some(15));
    assert_eq!(owner.birth_state(), BirthState::Gone);
    assert_eq!(child.terminate().await.unwrap(), status);
    assert!(
        agentchat_hosts::claude::process::ProcessIdentity::capture(std::process::id())
            .unwrap()
            .is_current()
    );
}

#[test]
#[ignore = "owned intermediate process exercises inherited actor stripping"]
fn inherited_context_parent() {
    assert!(std::env::var_os("AGENTCHAT_CONTEXT").is_some());
    let root = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_CONTEXT_TEST_ROOT").unwrap());
    let report = root.join("scope.json");
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut child = NativeChild::spawn_with_startup_gate(
            &std::env::current_exe().unwrap(),
            &root,
            &[
                "--ignored".into(),
                "--exact".into(),
                "managed_scope_child".into(),
                "--nocapture".into(),
            ],
            &BTreeMap::from([(
                "AGENTCHAT_NATIVE_CHILD_REPORT".into(),
                report.as_os_str().into(),
            )]),
            uuid::Uuid::new_v4(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !report.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        std::fs::write(report.with_extension("stop"), b"stop").unwrap();
        assert!(child.wait().await.unwrap().success());
    });
}
#[test]
fn fresh_native_child_does_not_inherit_its_callers_actor_context() {
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "inherited_context_parent",
            "--nocapture",
        ])
        .env("AGENTCHAT_CONTEXT", uuid::Uuid::new_v4().to_string())
        .env("AGENTCHAT_CONTEXT_TEST_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("scope.json")).unwrap()).unwrap();
    assert_eq!(report["inherited_context"], false);
}

#[test]
#[ignore = "explicit PTY acceptance invokes this parent with stdout redirected"]
fn controlling_terminal_parent() {
    use std::io::IsTerminal;
    assert!(!std::io::stdout().is_terminal());
    let root = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_CONTEXT_TEST_ROOT").unwrap());
    let report = root.join("terminal-scope.json");
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut child = NativeChild::spawn_with_startup_gate_terminal(
            &std::env::current_exe().unwrap(),
            &root,
            &[
                "--ignored".into(),
                "--exact".into(),
                "managed_scope_child".into(),
                "--nocapture".into(),
            ],
            &BTreeMap::from([
                (
                    "AGENTCHAT_NATIVE_CHILD_REPORT".into(),
                    report.as_os_str().into(),
                ),
                ("AGENTCHAT_NATIVE_TTY_MARKER".into(), "1".into()),
            ]),
            uuid::Uuid::new_v4(),
            agentchat_hosts::launch::NativeTerminal::Controlling,
        )
        .await
        .unwrap();
        let value: serde_json::Value = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = std::fs::read(&report)
                    && let Ok(value) = serde_json::from_slice(&bytes)
                {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        for field in ["stdin_tty", "stdout_tty", "stderr_tty"] {
            assert_eq!(value[field], true);
        }
        assert_ne!(value["stdin_device"], "/dev/tty");
        assert!(value["stdin_device"].as_str().unwrap().starts_with("/dev/"));
        println!("AGENTCHAT_PARENT_STDOUT_MARKER");
        std::fs::write(report.with_extension("stop"), b"stop").unwrap();
        assert!(child.wait().await.unwrap().success());
    });
}
