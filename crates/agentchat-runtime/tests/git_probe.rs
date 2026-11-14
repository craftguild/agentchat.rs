//! Isolated process environment proves Git-free discovery without bypassing Git.
mod support;

use agentchat_core::{ErrorCode, project::Scope};
use agentchat_runtime::project::ProjectManager;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use support::Fixture;

fn path(input: &Value, key: &str) -> PathBuf {
    PathBuf::from(input[key].as_str().unwrap())
}
fn git_binary() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("git"))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .unwrap()
        .canonicalize()
        .unwrap()
}

#[tokio::test]
#[ignore = "isolated PATH helper explicitly exercised in both modes by its parent"]
async fn isolated_git_probe_child() {
    for (actual, expected) in [
        ("HOME", "AGENTCHAT_EXPECTED_HOME"),
        ("CODEX_HOME", "AGENTCHAT_EXPECTED_CODEX_HOME"),
    ] {
        assert_eq!(std::env::var_os(actual), std::env::var_os(expected));
    }
    let input: Value =
        serde_json::from_str(&std::env::var("AGENTCHAT_GIT_PROBE_INPUT").unwrap()).unwrap();
    let manager = ProjectManager::from_environment().unwrap();
    let outer = manager
        .discover(path(&input, "workspace"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outer.identity().id.to_string(), input["project_id"]);
    assert_eq!(outer.identity().scope, Scope::Directory);
    let log = PathBuf::from(std::env::var_os("AGENTCHAT_GIT_PROBE_LOG").unwrap());
    if std::env::var("AGENTCHAT_GIT_PROBE_MODE").unwrap() == "absent" {
        assert_eq!(
            manager
                .discover(path(&input, "bare"))
                .await
                .unwrap_err()
                .code,
            ErrorCode::ConfigurationConflict
        );
        assert!(!log.exists());
        return;
    }
    // This first assertion fails with the original unconditional Git lookup.
    assert!(!log.exists(), "Ordinary directory discovery executed Git");
    let changing = path(&input, "changing");
    assert_eq!(manager.discover(&changing).await.unwrap().unwrap(), outer);
    assert!(!log.exists());
    let mut init = Command::new(path(&input, "real_git"));
    init.arg("-c")
        .arg(format!(
            "core.hooksPath={}",
            path(&input, "hooks").display()
        ))
        .arg("-c")
        .arg(format!(
            "init.templateDir={}",
            path(&input, "hooks").display()
        ))
        .arg("-C")
        .arg(&changing)
        .args(["init", "--quiet"]);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
    ] {
        init.env_remove(key);
    }
    assert!(init.status().unwrap().success());
    // No negative cache may conceal a newly created repository boundary.
    assert!(manager.discover(&changing).await.unwrap().is_none());
    assert!(log.exists());
    for key in ["repository", "linked", "bare"] {
        let before = fs::metadata(&log).unwrap().len();
        assert!(manager.discover(path(&input, key)).await.unwrap().is_none());
        assert!(
            fs::metadata(&log).unwrap().len() > before,
            "Git was bypassed for {key}"
        );
    }
    // HEAD can be an unrelated filename. Git remains authoritative when present.
    let before = fs::metadata(&log).unwrap().len();
    assert_eq!(
        manager
            .discover(path(&input, "ordinary_head"))
            .await
            .unwrap()
            .unwrap(),
        outer
    );
    assert!(fs::metadata(&log).unwrap().len() > before);
    let rejected = manager
        .discover(path(&input, "rejected"))
        .await
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::ConfigurationConflict);
    assert!(
        rejected.details["git_error"]
            .as_str()
            .unwrap()
            .contains("simulated ownership refusal")
    );
    assert_eq!(
        manager
            .discover(path(&input, "malformed"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ConfigurationConflict
    );
    assert!(!path(&input, "malformed").join(".agentchat").exists());
    // A Git process can close its pipes before exiting. EOF notifications must
    // never become an unbounded wait or be mistaken for a successful exit.
    assert_eq!(
        manager
            .discover(path(&input, "stall"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert!(!path(&input, "stall").join(".agentchat").exists());
}

fn run_child(f: &Fixture, input: &Value, mode: &str, bin: &Path, log: &Path) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "isolated_git_probe_child",
            "--nocapture",
        ])
        .env("AGENTCHAT_HOME", f.manager.home())
        .env("AGENTCHAT_USER_CONFIG_ROOT", &f.config)
        .env(
            "AGENTCHAT_GIT_PROBE_INPUT",
            serde_json::to_string(input).unwrap(),
        )
        .env("AGENTCHAT_GIT_PROBE_MODE", mode)
        .env("AGENTCHAT_GIT_PROBE_LOG", log)
        .env("AGENTCHAT_GIT_PROBE_REAL", path(input, "real_git"))
        .env("AGENTCHAT_GIT_PROBE_REJECT", path(input, "rejected"))
        .env("AGENTCHAT_GIT_PROBE_STALL", path(input, "stall"))
        .env("GIT_DIR", path(input, "bare"))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let paths = if mode == "absent" {
        vec![bin.to_owned()]
    } else {
        std::iter::once(bin.to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap()))
            .collect()
    };
    command.env("PATH", std::env::join_paths(paths).unwrap());
    for (actual, expected) in [
        ("HOME", "AGENTCHAT_EXPECTED_HOME"),
        ("CODEX_HOME", "AGENTCHAT_EXPECTED_CODEX_HOME"),
    ] {
        if let Some(value) = std::env::var_os(actual) {
            command.env(expected, value);
        } else {
            command.env_remove(expected);
        }
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "Git probe child failed in mode {mode}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Git probe child exceeded its deadline in mode {mode}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test]
async fn non_git_resolution_skips_processes_but_preserves_new_bare_linked_and_refused_boundaries() {
    let f = Fixture::new();
    let outer = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap();
    let repository = f.workspace.join("repository");
    f.repository(&repository);
    let bare = f.workspace.join("archive");
    fs::create_dir(&bare).unwrap();
    f.git(&bare, &["init", "--quiet", "--bare"]);
    let linked = f.workspace.join("linked");
    f.git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "probe-linked",
            linked.to_str().unwrap(),
        ],
    );
    assert!(linked.join(".git").is_file());
    assert!(!bare.join(".git").exists());
    let changing = f.workspace.join("changing");
    fs::create_dir(&changing).unwrap();
    let ordinary_head = f.workspace.join("ordinary-head");
    fs::create_dir(&ordinary_head).unwrap();
    fs::write(ordinary_head.join("HEAD"), "ordinary user data\n").unwrap();
    let rejected = f.workspace.join("rejected");
    f.repository(&rejected);
    let stall = f.workspace.join("stall");
    f.repository(&stall);
    let malformed = f.workspace.join("malformed");
    fs::create_dir(&malformed).unwrap();
    fs::write(malformed.join(".git"), "foreign malformed gitfile\n").unwrap();
    let bin = f.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let shim = bin.join("git");
    fs::write(
        &shim,
        r#"#!/bin/sh
printf 'git\n' >> "$AGENTCHAT_GIT_PROBE_LOG"
if [ "${GIT_DIR+x}" = x ]; then
    printf 'unexpected inherited Git routing\n' >&2
    exit 87
fi
if [ "$2" = "$AGENTCHAT_GIT_PROBE_REJECT" ]; then
    printf 'fatal: simulated ownership refusal\n' >&2
    exit 128
fi
if [ "$2" = "$AGENTCHAT_GIT_PROBE_STALL" ]; then
    exec 1>/dev/null 2>/dev/null
    exec /bin/sleep 6
fi
exec "$AGENTCHAT_GIT_PROBE_REAL" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let input = json!({"workspace":f.workspace,"project_id":outer.project.identity().id,
        "repository":repository,"bare":bare,"linked":linked,"changing":changing,
        "ordinary_head":ordinary_head,"rejected":rejected,"malformed":malformed,
        "stall":stall,
        "real_git":git_binary(),"hooks":f.hooks});
    run_child(
        &f,
        &input,
        "normal",
        &bin,
        &f.root.path().join("normal-calls"),
    );
    let absent = f.root.path().join("no-git");
    fs::create_dir(&absent).unwrap();
    run_child(
        &f,
        &input,
        "absent",
        &absent,
        &f.root.path().join("absent-calls"),
    );
}
