//! Real exits during chunk copying and create-new publication preserve the source.
#[path = "backup_crash/fixture.rs"]
mod fixture;
#[path = "backup_crash/process.rs"]
mod process;
mod support;
use agentchat_core::ErrorCode;
use fixture::Dataset;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    time::Duration,
};

#[tokio::test]
async fn twenty_six_real_backup_exits_preserve_snapshot_source_and_retry() {
    for cut in [
        "StagingCreated",
        "SnapshotAnchored",
        "EventSkeletons:16",
        "EventSkeletons:32",
        "Actors:2",
        "ProjectDefaultChannels:1",
        "MessageRoots:16",
        "MessageRoots:32",
        "MessageReplies:16",
        "Events:16",
        "Events:32",
        "Mentions:16",
        "ReadReceipts:16",
        "DeliveryTimeoutNotices:16",
        "WakeBatches:1",
        "Deliveries:16",
        "Deliveries:32",
        "Reservations:16",
        "Idempotency:16",
        "Sequences:1",
        "DestinationCommitted",
        "Validated",
        "Closed",
        "FileSynced",
        "Linked",
        "DirectorySynced",
    ] {
        let d = Dataset::new().await;
        let root = d.f.root.path();
        let before_counts = d.f.counts().await;
        let before_sequence = d.source_sequence().await;
        let mut child = process::Process::spawn(root, d.f.store.identity().id, cut, false);
        let mut wrote = false;
        let mut expected_source = None;
        let mut expected_counts = None;
        let mut expected_sequence = None;
        let status = loop {
            if let Some(status) = child.poll() {
                break status;
            }
            if !wrote && root.join("ready-snapshot").exists() {
                assert_eq!(d.f.counts().await, before_counts, "{cut}");
                assert_eq!(d.capture(&d.f.app).await, d.baseline, "{cut}");
                assert_eq!(d.source_sequence().await, before_sequence, "{cut}");
                // A committed writer while the child's source snapshot is held
                // proves the ordinary backup does not serialize source writes.
                d.late_write().await;
                expected_source = Some(d.capture(&d.f.app).await);
                expected_counts = Some(d.f.counts().await);
                expected_sequence = Some(d.source_sequence().await);
                wrote = true;
                fs::write(root.join("continue-snapshot"), b"continue").unwrap();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(status.code(), Some(86), "{cut}");
        let stages = process::stages(root);
        assert_eq!(stages.last().unwrap()["key"], cut);
        if !wrote {
            assert_eq!(cut, "StagingCreated");
            assert_eq!(d.f.counts().await, before_counts);
            assert_eq!(d.capture(&d.f.app).await, d.baseline);
            assert_eq!(d.source_sequence().await, before_sequence);
            d.late_write().await;
            expected_source = Some(d.capture(&d.f.app).await);
            expected_counts = Some(d.f.counts().await);
            expected_sequence = Some(d.source_sequence().await);
        }
        let source = expected_source.unwrap();
        let source_counts = expected_counts.unwrap();
        let source_sequence = expected_sequence.unwrap();
        assert_eq!(d.capture(&d.f.app).await, source, "{cut}");
        assert_eq!(d.f.counts().await, source_counts, "{cut}");
        assert_eq!(d.source_sequence().await, source_sequence, "{cut}");
        d.f.store.integrity_check().await.unwrap();
        d.replay_source().await;
        assert_eq!(d.f.counts().await, source_counts, "{cut}");
        assert_eq!(d.source_sequence().await, source_sequence, "{cut}");
        let destination = root.join("backup.sqlite3");
        let published = matches!(cut, "Linked" | "DirectorySynced");
        assert_eq!(
            fs::symlink_metadata(&destination).is_ok(),
            published,
            "{cut}"
        );
        let orphans = process::staging(root);
        assert_eq!(orphans.len(), 1, "{cut}");
        assert_eq!(
            fs::metadata(&orphans[0]).unwrap().permissions().mode() & 0o777,
            0o700
        );
        if published {
            assert_eq!(
                fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert!(!root.join("backup.sqlite3-wal").exists());
            let bytes = fs::read(&destination).unwrap();
            assert_eq!(
                d.f.store.backup(&destination).await.unwrap_err().code,
                ErrorCode::ConfigurationConflict
            );
            assert_eq!(fs::read(&destination).unwrap(), bytes);
            d.restore(&destination, &d.baseline).await;
        }
        // A new attempt ignores the private orphan and captures the later write.
        let retry = if published {
            root.join("retry.sqlite3")
        } else {
            destination
        };
        d.f.store.backup(&retry).await.unwrap();
        d.restore(&retry, &source).await;
        assert_eq!(
            process::staging(root),
            orphans,
            "orphan must not be guessed safe to delete"
        );
        assert_eq!(d.capture(&d.f.app).await, source, "{cut}");
        assert_eq!(d.f.counts().await, source_counts, "{cut}");
        assert_eq!(d.source_sequence().await, source_sequence, "{cut}");
    }
}

#[tokio::test]
async fn publication_races_preserve_foreign_file_directory_and_dangling_symlink() {
    for kind in ["file", "directory", "symlink"] {
        let d = Dataset::new().await;
        let root = d.f.root.path();
        let mut child = process::Process::spawn(root, d.f.store.identity().id, "", true);
        let mut snapshot = false;
        let mut collision = false;
        let destination = root.join("backup.sqlite3");
        let status = loop {
            if let Some(status) = child.poll() {
                break status;
            }
            if !snapshot && root.join("ready-snapshot").exists() {
                snapshot = true;
                fs::write(root.join("continue-snapshot"), b"continue").unwrap();
            }
            if !collision && root.join("ready-publish").exists() {
                match kind {
                    "file" => fs::write(&destination, b"foreign backup").unwrap(),
                    "directory" => fs::create_dir(&destination).unwrap(),
                    "symlink" => symlink("missing-foreign-target", &destination).unwrap(),
                    _ => unreachable!(),
                }
                collision = true;
                fs::write(root.join("continue-publish"), b"continue").unwrap();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(status.code(), Some(0), "{kind}");
        assert!(snapshot && collision);
        assert_eq!(process::stages(root).last().unwrap()["key"], "FileSynced");
        assert!(process::staging(root).is_empty());
        match kind {
            "file" => assert_eq!(fs::read(&destination).unwrap(), b"foreign backup"),
            "directory" => assert!(fs::read_dir(&destination).unwrap().next().is_none()),
            "symlink" => assert_eq!(
                fs::read_link(&destination).unwrap(),
                std::path::Path::new("missing-foreign-target")
            ),
            _ => unreachable!(),
        }
        assert_eq!(d.capture(&d.f.app).await, d.baseline);
        d.f.store.integrity_check().await.unwrap();
        d.late_write().await;
    }
}
