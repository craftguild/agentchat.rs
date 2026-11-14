//! The ordinary daemon collects proved dead-writer staging on existing housekeeping.
mod support;

use agentchat_hosts::staging::{FilePlan, OriginRegistry, StagePurpose};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

#[tokio::test]
async fn housekeeping_collects_a_proved_dead_writer_origin() {
    let running = support::runtime::Running::new(support::runtime::options()).await;
    let report = running.f.root.path().join("orphan-stage-report");
    let status = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "crashed_staging_origin_fixture"])
        .env("AGENTCHAT_STAGING_FIXTURE_HOME", running.f.manager.home())
        .env("AGENTCHAT_STAGING_FIXTURE_PARENT", &running.f.workspace)
        .env("AGENTCHAT_STAGING_FIXTURE_REPORT", &report)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap();
    assert_eq!(status.code(), Some(37));
    let stage = PathBuf::from(fs::read_to_string(&report).unwrap());
    assert!(stage.exists());
    assert!(
        fs::read_dir(running.f.manager.home().join("staging"))
            .unwrap()
            .next()
            .is_some()
    );

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !stage.exists()
                && fs::read_dir(running.f.manager.home().join("staging"))
                    .unwrap()
                    .next()
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    running.stop().await;
}

#[tokio::test]
#[ignore = "owned subprocess fixture, explicitly invoked by the parent case"]
async fn crashed_staging_origin_fixture() {
    let home = PathBuf::from(std::env::var_os("AGENTCHAT_STAGING_FIXTURE_HOME").unwrap());
    let parent = PathBuf::from(std::env::var_os("AGENTCHAT_STAGING_FIXTURE_PARENT").unwrap());
    let report = PathBuf::from(std::env::var_os("AGENTCHAT_STAGING_FIXTURE_REPORT").unwrap());
    let stage = format!(".runtime-orphan-{}", uuid::Uuid::new_v4());
    let target = format!("runtime-target-{}", uuid::Uuid::new_v4());
    let mut origin = OriginRegistry::new(&home)
        .begin_file(FilePlan {
            parent: &parent,
            stage: stage.as_ref(),
            target: target.as_ref(),
            purpose: StagePurpose::Metadata,
            alternate_parent: None,
            guard: None,
        })
        .unwrap();
    let path = parent.join(&stage);
    let bytes = b"proved dead-writer stage\n";
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(bytes).unwrap();
    origin.seal(&file, bytes).unwrap();
    fs::write(report, path.as_os_str().as_encoded_bytes()).unwrap();
    std::process::exit(37);
}
