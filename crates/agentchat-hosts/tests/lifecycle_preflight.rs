//! Disposable proof that removal cannot replace the filesystem coordination inode.

use agentchat_core::ErrorCode;
use agentchat_hosts::lifecycle::Lifecycle;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    process::{Command, Stdio},
    time::Duration,
};

#[test]
fn fence_survives_state_replacement_and_recovery_metadata_remains_private() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap().join("state");
    fs::create_dir(&root).unwrap();
    let lifecycle = Lifecycle::new(&root).unwrap();
    let fence = lifecycle.exclusive().unwrap();
    let original = fs::metadata(lifecycle.directory().join("fence.lock"))
        .unwrap()
        .ino();
    let journal = lifecycle.directory().join("removal.json");
    fs::write(&journal, b"surviving recovery fixture").unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_dir_all(&root).unwrap();
    fs::create_dir(&root).unwrap();
    fence.validate().unwrap();
    let replacement = Lifecycle::new(&root).unwrap();
    assert_eq!(lifecycle.directory(), replacement.directory());
    assert_eq!(
        fs::metadata(replacement.directory().join("fence.lock"))
            .unwrap()
            .ino(),
        original
    );
    assert_eq!(
        replacement
            .acquire(false, Duration::ZERO)
            .err()
            .unwrap()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(fs::read(&journal).unwrap(), b"surviving recovery fixture");
    assert_eq!(
        fs::metadata(lifecycle.directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    drop(fence);
    replacement.shared().unwrap();
}

#[test]
fn independent_process_cannot_enter_after_root_replacement() {
    let fixture = tempfile::tempdir().unwrap();
    let base = fixture.path().canonicalize().unwrap();
    let root = base.join("state");
    let config = base.join("config");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&config).unwrap();
    let lifecycle = Lifecycle::new(&root).unwrap();
    let fence = lifecycle.exclusive().unwrap();
    fs::remove_dir_all(&root).unwrap();
    fs::create_dir(&root).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "lifecycle_child", "--nocapture"])
        .env("AGENTCHAT_HOME", &root)
        .env("AGENTCHAT_USER_CONFIG_ROOT", &config)
        .env("AGENTCHAT_LIFECYCLE_PREFLIGHT", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("OS_FENCE_BUSY"));
    fence.validate().unwrap();
}

#[test]
#[ignore = "subprocess entrypoint, invoked by independent_process_cannot_enter_after_root_replacement"]
fn lifecycle_child() {
    assert_eq!(std::env::var("AGENTCHAT_LIFECYCLE_PREFLIGHT").unwrap(), "1");
    let root = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_HOME").unwrap());
    let config = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_USER_CONFIG_ROOT").unwrap());
    assert_eq!(root.parent(), config.parent());
    let lifecycle = Lifecycle::new(&root).unwrap();
    assert_eq!(
        lifecycle
            .acquire(false, Duration::from_millis(50))
            .err()
            .unwrap()
            .code,
        ErrorCode::Busy
    );
    println!("OS_FENCE_BUSY");
}

#[test]
fn replaced_or_symbolic_fence_and_public_recovery_directory_are_refused() {
    let fixture = tempfile::tempdir().unwrap();
    let base = fixture.path().canonicalize().unwrap();
    let lifecycle = Lifecycle::new(&base.join("state")).unwrap();
    let fence = lifecycle.exclusive().unwrap();
    let path = lifecycle.directory().join("fence.lock");
    fs::rename(&path, lifecycle.directory().join("old-lock")).unwrap();
    fs::write(&path, b"").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        fence.validate().err().unwrap().code,
        ErrorCode::ConfigurationConflict
    );
    fs::remove_file(&path).unwrap();
    symlink(lifecycle.directory().join("old-lock"), &path).unwrap();
    assert!(lifecycle.shared().is_err());
    fs::remove_file(&path).unwrap();
    drop(fence);
    fs::set_permissions(lifecycle.directory(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        lifecycle.shared().err().unwrap().code,
        ErrorCode::ConfigurationConflict
    );
}
