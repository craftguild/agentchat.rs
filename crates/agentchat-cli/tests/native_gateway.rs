//! Actual agentchat mcp/namespace subprocesses with a synthetic native parent. No host admission.
mod support;
use agentchat_hosts::claude::{namespace::OwnerNamespace, process::ProcessIdentity};
use serde_json::{Value, json};
use std::process::Stdio;
use support::{Fixture, mcp::Mcp};
use uuid::Uuid;
fn configured(f: &Fixture) -> tokio::process::Command {
    let mut cmd = f.command(&["mcp"]);
    cmd.env(
        "AGENTCHAT_CLAUDE_NATIVE_EXE",
        std::env::current_exe().unwrap(),
    )
    .env("AGENTCHAT_CLAUDE_NATIVE_VERSION", "test-observed")
    .env("CLAUDE_CODE_SESSION_ID", Uuid::new_v4().to_string())
    .env("AGENTCHAT_CONTEXT", Uuid::new_v4().to_string())
    .stdin(Stdio::piped());
    cmd
}
#[tokio::test]
async fn native_gateway_tick_collects_a_dead_owner_without_initializing_or_borrowing_it() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::time::{Duration, Instant};
    let f = Fixture::new();
    let mut owner = tokio::process::Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Observe the actual fixture executable before capturing its full owner;
    // an immediate post-spawn observation can still see a startup transition.
    let ready = format!("{}\n", Uuid::new_v4());
    owner
        .stdin
        .as_mut()
        .unwrap()
        .write_all(ready.as_bytes())
        .await
        .unwrap();
    let mut output = BufReader::new(owner.stdout.take().unwrap());
    let mut echo = String::new();
    tokio::time::timeout(Duration::from_secs(10), output.read_line(&mut echo))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(echo, ready);
    let dead = OwnerNamespace::open(
        f.manager.home(),
        ProcessIdentity::capture(owner.id().unwrap()).unwrap(),
    )
    .await
    .unwrap();
    let mut client = Mcp::from_child(configured(&f).spawn().unwrap()).await;
    let live = OwnerNamespace::open(
        f.manager.home(),
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .await
    .unwrap();
    let header = std::fs::read(live.directory().join("owner-state.json")).unwrap();
    drop(owner.stdin.take());
    assert!(owner.wait().await.unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let owners = dead.directory().parent().unwrap();
    let key = dead.directory().file_name().unwrap().to_str().unwrap();
    while dead.directory().exists()
        || owners.join(format!(".retired-{key}")).exists()
        || owners.join(format!(".gc-{key}.json")).exists()
    {
        assert!(
            Instant::now() < deadline,
            "gateway maintenance did not finish"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        std::fs::read(live.directory().join("owner-state.json")).unwrap(),
        header
    );
    assert_eq!(
        client
            .call("agentchat_whoami", json!({"project_path":f.workspace}))
            .await["error"]["code"],
        "session_required"
    );
    client.close().await;
    assert!(
        !f.workspace.join(".agentchat").exists() && !f.manager.home().join("contexts").exists()
    );
    assert!(std::fs::read_dir(&f.config).unwrap().next().is_none());
}
#[tokio::test]
async fn explicit_native_mcp_lists_tools_but_never_borrows_missing_call_authority() {
    let f = Fixture::new();
    let parent = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut client = Mcp::from_child(configured(&f).spawn().unwrap()).await;
    let listed = client.request("tools/list", json!({})).await;
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 23);
    for arguments in [
        json!({"project_path":f.workspace}),
        json!({"project_path":f.workspace,"context_id":Uuid::new_v4()}),
    ] {
        let result = client.call("agentchat_whoami", arguments).await;
        assert_eq!(result["error"]["code"], "session_required", "{result}");
    }
    assert!(!f.workspace.join(".agentchat").exists());
    assert!(!f.manager.home().join("contexts").exists());
    let namespace = OwnerNamespace::open(f.manager.home(), parent)
        .await
        .unwrap();
    let endpoints = std::fs::read_dir(namespace.directory())
        .unwrap()
        .filter_map(|x| x.ok())
        .filter(|x| {
            x.file_name().to_string_lossy().starts_with("gateway-")
                && x.path().extension().is_some_and(|x| x == "json")
        })
        .map(|x| x.path())
        .collect::<Vec<_>>();
    assert_eq!(endpoints.len(), 1);
    client.close().await;
    assert!(!endpoints[0].exists());
    assert!(!f.workspace.join(".agentchat").exists());
    assert!(!f.manager.home().join("contexts").exists());
    assert!(std::fs::read_dir(&f.config).unwrap().next().is_none());
}
#[tokio::test]
async fn native_namespace_helper_and_mcp_choose_the_same_complete_kernel_owner() {
    let f = Fixture::new();
    let parent = std::env::current_exe().unwrap();
    let output = f
        .command(&[
            "internal",
            "claude-namespace",
            "--native-executable",
            parent.to_str().unwrap(),
            "--native-version",
            "test-observed",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(answer.get("context_id").is_none() && answer.get("project_id").is_none());
    let expected = OwnerNamespace::open(
        f.manager.home(),
        ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(answer["directory"], expected.directory().to_str().unwrap());
    assert_eq!(answer["owner_root"], expected.id().to_string());
    assert_eq!(answer["observer"], expected.observer());
    let mut client = Mcp::from_child(configured(&f).spawn().unwrap()).await;
    assert_eq!(
        client
            .call("agentchat_whoami", json!({"project_path":f.workspace}))
            .await["error"]["code"],
        "session_required"
    );
    client.close().await;
    assert!(
        !f.workspace.join(".agentchat").exists() && !f.manager.home().join("contexts").exists()
    );
}
#[tokio::test]
async fn a_wrong_native_ancestor_or_legacy_restore_refuses_before_creating_state() {
    for restore in [false, true] {
        let f = Fixture::new();
        let mut cmd = configured(&f);
        if restore {
            cmd.arg("--restore-context").arg(Uuid::new_v4().to_string());
        } else {
            cmd.env(
                "AGENTCHAT_CLAUDE_NATIVE_EXE",
                env!("CARGO_BIN_EXE_agentchat"),
            )
            .env("AGENTCHAT_CLAUDE_NATIVE_VERSION", "test-observed");
        }
        let result = cmd.output().await.unwrap();
        assert!(!result.status.success() && result.stdout.is_empty() && !result.stderr.is_empty());
        assert!(!f.manager.home().exists());
    }
}
#[tokio::test]
async fn spawn_session_hints_do_not_select_a_native_root_or_prevent_transport_startup() {
    for locator in [
        None,
        Some("not-a-uuid"),
        Some("00000000-0000-0000-0000-000000000000"),
    ] {
        let f = Fixture::new();
        let mut cmd = configured(&f);
        cmd.env_remove("CLAUDE_CODE_SESSION_ID");
        if let Some(v) = locator {
            cmd.env("CLAUDE_CODE_SESSION_ID", v);
        }
        let mut client = Mcp::from_child(cmd.spawn().unwrap()).await;
        assert_eq!(
            client
                .call("agentchat_whoami", json!({"project_path":f.workspace}))
                .await["error"]["code"],
            "session_required"
        );
        let ns = OwnerNamespace::open(
            f.manager.home(),
            ProcessIdentity::capture(std::process::id()).unwrap(),
        )
        .await
        .unwrap();
        assert!(
            agentchat_cli::claude_gateway::records::endpoint(ns.directory(), ns.id()).is_file()
        );
        client.close().await;
        assert!(!f.manager.home().join("contexts").exists());
        assert!(!f.workspace.join(".agentchat").exists());
    }
}

#[test]
#[ignore = "synthetic native parent; the parent-exit test executes this child explicitly"]
fn native_owner_child() {
    assert_eq!(std::env::var("AGENTCHAT_TEST_NATIVE_OWNER").unwrap(), "1");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_agentchat"))
        .arg("mcp")
        .env(
            "AGENTCHAT_CLAUDE_NATIVE_EXE",
            std::env::current_exe().unwrap(),
        )
        .env("AGENTCHAT_CLAUDE_NATIVE_VERSION", "test-observed")
        .env("CLAUDE_CODE_SESSION_ID", Uuid::new_v4().to_string())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    std::fs::write(
        std::env::var_os("AGENTCHAT_TEST_MCP_PID").unwrap(),
        child.id().to_string(),
    )
    .unwrap();
    for _ in 0..4000 {
        if std::path::Path::new(&std::env::var_os("AGENTCHAT_TEST_OWNER_EXIT").unwrap()).exists() {
            // Deliberate process cut: no Child destructor/wait closes the inherited pipe.
            std::process::exit(86);
        }
        if child.try_wait().unwrap().is_some() {
            panic!("Synthetic native child exited before its owner cut");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("Synthetic native owner was never released");
}
struct ExactChild(ProcessIdentity);
impl Drop for ExactChild {
    fn drop(&mut self) {
        // Only the child whose full kernel identity was captured may be cleaned up.
        if self.0.is_current()
            && let Some(pid) = rustix::process::Pid::from_raw(self.0.pid() as i32)
        {
            let expected = format!("{} mcp", env!("CARGO_BIN_EXE_agentchat"));
            let command = std::process::Command::new("/bin/ps")
                .args(["-ww", "-p", &self.0.pid().to_string(), "-o", "command="])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok());
            if command.as_deref().is_some_and(|s| s.trim() == expected) && self.0.is_current() {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
        }
    }
}
#[tokio::test]
async fn physical_owner_exit_closes_native_mcp_even_while_a_grandparent_holds_stdin_open() {
    use std::time::Duration;
    use tokio::{
        io::AsyncReadExt,
        time::{Instant, timeout},
    };
    let f = Fixture::new();
    let pid_file = f.workspace.join("child.pid");
    let exit_file = f.workspace.join("owner.exit");
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "native_owner_child", "--nocapture"])
        .env("AGENTCHAT_TEST_NATIVE_OWNER", "1")
        .env("AGENTCHAT_TEST_MCP_PID", &pid_file)
        .env("AGENTCHAT_TEST_OWNER_EXIT", &exit_file)
        .env("AGENTCHAT_HOME", f.manager.home())
        .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
        .current_dir(&f.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut parent = command.spawn().unwrap();
    let retained_stdin = parent.stdin.take().unwrap();
    let mut stdout = parent.stdout.take().unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    while !pid_file.exists() {
        assert!(parent.try_wait().unwrap().is_none() && Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The child publishes this file from its own test main, after exec.
    let owner = ProcessIdentity::capture(parent.id().unwrap()).unwrap();
    let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    let exact = ExactChild(ProcessIdentity::capture(pid).unwrap());
    let namespace = OwnerNamespace::open(f.manager.home(), owner).await.unwrap();
    let endpoint = loop {
        let found = std::fs::read_dir(namespace.directory())
            .unwrap()
            .filter_map(|x| x.ok())
            .find(|x| {
                x.file_name().to_string_lossy().starts_with("gateway-")
                    && x.path().extension().is_some_and(|x| x == "json")
            });
        if let Some(file) = found {
            break file.path();
        }
        assert!(parent.try_wait().unwrap().is_none() && Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let published: agentchat_cli::claude_gateway::records::Endpoint =
        agentchat_cli::claude_gateway::records::read(&endpoint).unwrap();
    std::fs::write(exit_file, b"exit").unwrap();
    assert_eq!(
        timeout(Duration::from_secs(2), parent.wait())
            .await
            .unwrap()
            .unwrap()
            .code(),
        Some(86)
    );
    let mut ignored_test_harness_prelude = Vec::new();
    timeout(
        Duration::from_secs(3),
        stdout.read_to_end(&mut ignored_test_harness_prelude),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!exact.0.is_current());
    assert!(!endpoint.exists());
    assert!(!published.socket.parent().unwrap().exists());
    drop(retained_stdin);
    drop(exact);
    assert!(
        !f.workspace.join(".agentchat").exists() && !f.manager.home().join("contexts").exists()
    );
}

#[tokio::test]
async fn callback_profile_must_match_the_actual_gateway_selection_before_bootstrap_activation() {
    use agentchat_cli::claude_gateway::records::{
        self, Control, Endpoint, LifecycleOwner, Request, Response,
    };
    use agentchat_hosts::claude::{NativeBootstrap, NativeLifecycle};
    use agentchat_protocol::framing;
    let f = Fixture::new();
    let hint = Uuid::new_v4();
    let mut command = configured(&f);
    command.env("CLAUDE_CODE_SESSION_ID", hint.to_string());
    let mut client = Mcp::from_child(command.spawn().unwrap()).await;
    let parent = ProcessIdentity::capture(std::process::id()).unwrap();
    let namespace = OwnerNamespace::open(f.manager.home(), parent.clone())
        .await
        .unwrap();
    let endpoint: Endpoint =
        records::read(&records::endpoint(namespace.directory(), namespace.id())).unwrap();
    let intent = namespace
        .reserve(hint, f.workspace.clone(), Some(endpoint.nonce))
        .await
        .unwrap();
    let expected_os = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    for (version, os, accepted) in [
        ("2.1.300", expected_os.as_str(), false),
        ("test-observed", "another-os", false),
        ("test-observed", expected_os.as_str(), true),
    ] {
        let registration = LifecycleOwner {
            owner: parent.clone(),
            registration: NativeLifecycle::Bootstrap(NativeBootstrap {
                owner_root: namespace.id(),
                observer: namespace.observer().into(),
                generation: intent.generation,
                root: hint,
                lifecycle_id: Uuid::new_v4(),
                host_version: version.into(),
                os_profile: os.into(),
                working_directory: f.workspace.clone(),
            }),
        };
        if accepted {
            namespace
                .commit(intent.clone(), registration.registration.clone())
                .await
                .unwrap();
        }
        let mut socket = tokio::net::UnixStream::connect(&endpoint.socket)
            .await
            .unwrap();
        framing::write(
            &mut socket,
            &Request {
                nonce: endpoint.nonce,
                control: Control::Register {
                    registration: Box::new(registration),
                },
            },
        )
        .await
        .unwrap();
        let response: Response = framing::read(&mut tokio::io::BufReader::new(socket))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.accepted, accepted);
        if !accepted {
            assert_eq!(
                response.error,
                Some(agentchat_core::ErrorCode::UnsupportedHost)
            );
        }
    }
    assert_eq!(
        client
            .call("agentchat_whoami", json!({"project_path":f.workspace}))
            .await["error"]["code"],
        "session_required"
    );
    client.close().await;
    assert!(
        !f.workspace.join(".agentchat").exists() && !f.manager.home().join("contexts").exists()
    );
}

#[tokio::test]
async fn invalid_gateway_configuration_reports_stable_errors_before_publication() {
    use agentchat_cli::claude_gateway::{Configuration, serve};
    use agentchat_hosts::claude::observer::NativeProfile;
    use std::sync::Arc;
    for (version, prefix, hint, code) in [
        (
            "unsafe\nversion",
            "mcp__agentchat__",
            Uuid::new_v4(),
            agentchat_core::ErrorCode::UnsupportedHost,
        ),
        (
            "2.1.301",
            "mcp__agentchat\n__",
            Uuid::new_v4(),
            agentchat_core::ErrorCode::InvalidArgument,
        ),
        (
            "2.1.301",
            "mcp__agentchat__",
            Uuid::nil(),
            agentchat_core::ErrorCode::InvalidArgument,
        ),
    ] {
        let f = Fixture::new();
        let backend = Arc::new(agentchat_cli::operations(
            f.manager.clone(),
            env!("CARGO_BIN_EXE_agentchat").into(),
        ));
        let config = Configuration {
            directory: f.workspace.clone(),
            profile: NativeProfile {
                executable: std::env::current_exe().unwrap(),
                version: version.into(),
            },
            owner_hint: hint,
            owner_cursor: None,
            tool_prefix: prefix.into(),
        };
        let error = serve(config, backend, f.manager.clone(), None)
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<agentchat_core::Error>().unwrap().code,
            code
        );
        assert!(!f.manager.home().exists());
        assert!(std::fs::read_dir(&f.workspace).unwrap().next().is_none());
    }
}
