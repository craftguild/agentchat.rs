//! Observation is bounded and cannot execute a PATH launcher as the native image.
use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn version_requires_one_bounded_control_free_engine_line() {
    assert_eq!(decode(b"2.1.302 (Claude Code)\n").unwrap(), "2.1.302");
    assert_eq!(
        decode(b"next-canary (Claude Code)\n").unwrap(),
        "next-canary"
    );
    let oversized = format!("{} (Claude Code)", "x".repeat(129));
    for bytes in [
        b"2.1.302".as_slice(),
        b"codex-cli 2.1.302",
        b"2.1.302\nforeign (Claude Code)",
        b"\xff",
        b"",
        b"2.1.302\0 (Claude Code)",
        oversized.as_bytes(),
    ] {
        assert_eq!(decode(bytes).unwrap_err().code, ErrorCode::UnsupportedHost);
    }
}

#[test]
fn absolute_path_selection_preserves_first_command_and_resolves_its_actual_image() {
    let temp = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    symlink(&executable, temp.path().join("claude")).unwrap();
    assert_eq!(
        locate(Some(temp.path().as_os_str().into())).unwrap(),
        executable
    );
    assert!(locate(Some("relative-only".into())).is_err());
    assert!(locate(None).is_err());
}

#[tokio::test]
async fn a_launcher_script_is_refused_before_version_execution() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("claude");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ntouch '{}'\nprintf '2.1.302 (Claude Code)\\n'\n",
            temp.path().join("executed").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        discover(Some(temp.path().as_os_str().into()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedHost
    );
    assert!(!temp.path().join("executed").exists());
}

#[tokio::test]
async fn output_limits_failures_and_timeout_reap_the_owned_direct_probe() {
    assert_eq!(
        decode(
            &output(Path::new("/bin/echo"), &["2.1.302 (Claude Code)"])
                .await
                .unwrap()
        )
        .unwrap(),
        "2.1.302"
    );
    assert!(output(Path::new("/bin/false"), &[]).await.is_err());
    assert!(
        output(Path::new("/bin/echo"), &[&"x".repeat(4097)])
            .await
            .is_err()
    );
    let start = std::time::Instant::now();
    assert!(output(Path::new("/bin/sleep"), &["30"]).await.is_err());
    assert!(start.elapsed() < Duration::from_secs(20));
}
