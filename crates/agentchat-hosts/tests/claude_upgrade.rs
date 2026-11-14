//! Actual executable replacement and original-owner snapshot boundaries.
use agentchat_hosts::{
    claude::{namespace::OwnerNamespace, observer::NativeObserver},
    install::Installer,
    launch::{NativeChild, Registry, StartupScope},
    setup::{Environment, Roots, ToolSetup},
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{DirBuilderExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use uuid::Uuid;

fn image(directory: &Path) -> PathBuf {
    let source = directory.join("native.rs");
    fs::write(&source, r#"
use std::{env,fs,process::{Command,Stdio},thread,time::{Duration,Instant}};
fn main() {
  if env::args().nth(1).as_deref()==Some("--version") {
    println!("{} (Claude Code)",env::current_exe().unwrap().file_name().unwrap().to_str().unwrap());
    return
  }
  let deadline=Instant::now()+Duration::from_secs(90);
  let go=env::var("UPGRADE_GO").unwrap();
  while Instant::now()<deadline {
    if fs::remove_file(&go).is_ok() {
      let status=Command::new(env::var("UPGRADE_TEST").unwrap())
        .args(["--exact","snapshot_helper","--nocapture"])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit()).status().unwrap();
      fs::write(env::var("UPGRADE_EXIT").unwrap(),status.code().unwrap_or(255).to_string()).unwrap();
    }
    thread::sleep(Duration::from_millis(10));
  }
}
"#).unwrap();
    let output = directory.join("native-template");
    assert!(
        Command::new("rustc")
            .args(["--edition=2024"])
            .arg(&source)
            .arg("-o")
            .arg(&output)
            .status()
            .unwrap()
            .success()
    );
    output
}

fn setup(root: &Path, binary: &Path, search: &Path) -> (ToolSetup, Roots) {
    let environment = Environment {
        user_config_root: Some(root.join("config")),
        claude_config_dir: Some(root.join("config/host-config")),
        ..Default::default()
    };
    let roots = environment.resolve().unwrap();
    (
        ToolSetup::new(
            root.join("state"),
            binary.into(),
            environment,
            Some(search.as_os_str().into()),
        ),
        roots,
    )
}

#[tokio::test]
async fn new_launch_uses_current_path_while_original_snapshot_survives_removed_a() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let template = image(&root);
    let a = root.join("version-A");
    let b = root.join("version-B");
    fs::copy(&template, &a).unwrap();
    fs::copy(&template, &b).unwrap();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    symlink(&a, bin.join("claude")).unwrap();
    let agentchat = root.join("agentchat");
    let (setup, roots) = setup(&root, &agentchat, &bin);
    let state = root.join("state");
    fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
    let installer = Installer::new(
        &state,
        roots.tool_roots(&[
            agentchat_core::model::HostKind::Codex,
            agentchat_core::model::HostKind::Claude,
        ]),
    )
    .unwrap();
    installer
        .install(roots.claude_managed_artifacts(&agentchat, &state).unwrap())
        .await
        .unwrap();
    let before = fs::read(&roots.claude_registry).unwrap();
    let plan = setup.claude_launch_plan().await.unwrap();
    assert!(plan.is_managed());
    assert_eq!(plan.profile().executable, a);
    assert_eq!(plan.profile().version, "version-A");
    let env = BTreeMap::from([
        (
            "UPGRADE_TEST".into(),
            std::env::current_exe().unwrap().into_os_string(),
        ),
        ("UPGRADE_GO".into(), root.join("go").into_os_string()),
        ("UPGRADE_EXIT".into(), root.join("exit").into_os_string()),
        ("UPGRADE_HOME".into(), state.as_os_str().into()),
        (
            "UPGRADE_RESULT".into(),
            root.join("result.json").into_os_string(),
        ),
    ]);
    let mut child = NativeChild::spawn_with_startup_gate(&a, &root, &[], &env, Uuid::new_v4())
        .await
        .unwrap();
    let registry = Registry::new(&state);
    registry
        .publish_claude_profile(&child, plan.profile())
        .unwrap();
    let scope = StartupScope {
        root: child.startup_root().unwrap(),
        generation: child.identity().generation,
    };
    let original = child.owner().clone();
    assert!(original.verify_ancestor().is_err());
    assert!(original.verify_parent().is_err());
    fs::remove_file(bin.join("claude")).unwrap();
    symlink(&b, bin.join("claude")).unwrap();
    fs::remove_file(&a).unwrap();
    let next = setup.claude_launch_plan().await.unwrap();
    assert_eq!(next.profile().executable, b);
    assert_eq!(next.profile().version, "version-B");
    let (owner, profile) = registry.selected_claude(&scope).unwrap();
    assert_eq!(owner, original);
    assert_eq!(profile.executable, a);
    assert_eq!(profile.version, "version-A");
    assert!(
        registry
            .selected_claude(&StartupScope {
                root: Uuid::new_v4(),
                ..scope.clone()
            })
            .is_err()
    );
    assert!(
        registry
            .selected_claude(&StartupScope {
                generation: Uuid::new_v4(),
                ..scope.clone()
            })
            .is_err()
    );
    assert!(
        registry
            .publish_claude_profile(&child, next.profile())
            .is_err()
    );
    fs::write(root.join("go"), b"go").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !root.join("exit").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(root.join("exit")).unwrap(), "0");
    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("result.json")).unwrap()).unwrap();
    assert_eq!(result["version"], "version-A");
    assert_eq!(result["pid"], original.pid());
    fs::remove_file(bin.join("claude")).unwrap();
    assert!(setup.claude_launch_plan().await.is_err()); // No saved A or B fallback.
    assert_eq!(fs::read(&roots.claude_registry).unwrap(), before);
    child.terminate().await.unwrap();
    assert!(registry.selected_claude(&scope).is_err());
}

#[test]
fn snapshot_helper() {
    let Some(home) = std::env::var_os("UPGRADE_HOME") else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (owner, profile) = agentchat_hosts::launch::managed_claude(Path::new(&home), true)
            .await
            .unwrap();
        owner.verify_ancestor().unwrap();
        assert!(!profile.executable.exists());
        let namespace = OwnerNamespace::open(Path::new(&home), owner.clone())
            .await
            .unwrap();
        namespace.publish_profile(profile.clone()).await.unwrap();
        NativeObserver::open_owned(namespace.directory().into(), profile.clone(), &owner)
            .await
            .unwrap();
        fs::write(
            std::env::var_os("UPGRADE_RESULT").unwrap(),
            serde_json::json!({"version":profile.version,"pid":owner.pid()}).to_string(),
        )
        .unwrap();
    });
}

#[test]
fn stable_owned_artifacts_contain_no_selected_engine_path_or_version() {
    let root = tempfile::tempdir().unwrap();
    let (_, roots) = setup(root.path(), Path::new("/test/agentchat"), root.path());
    let claude = roots
        .claude_managed_artifacts(Path::new("/test/agentchat"), &root.path().join("state"))
        .unwrap();
    let codex = roots
        .codex_managed_artifacts(Path::new("/test/agentchat"), &root.path().join("state"))
        .unwrap();
    let text = format!("{claude:?}");
    assert!(!text.contains("AGENTCHAT_CLAUDE_NATIVE_EXE"));
    assert!(!text.contains("AGENTCHAT_CLAUDE_NATIVE_VERSION"));
    assert!(text.contains("AGENTCHAT_CLAUDE_MANAGED"));
    let text = format!("{codex:?}");
    assert!(!text.contains("AGENTCHAT_CODEX_NATIVE_EXE"));
    assert!(!text.contains("AGENTCHAT_CODEX_NATIVE_VERSION"));
    assert!(text.contains("AGENTCHAT_CODEX_MANAGED"));
}
