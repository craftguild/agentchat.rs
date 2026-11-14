//! Publishing finishes a writer only after rename and durable parent sync.
use super::*;
use crate::staging::StagePurpose;
use fs2::FileExt;
use std::io::{Read, Seek, SeekFrom};

#[test]
fn a_published_genesis_transfers_the_original_locked_readable_description() {
    let root = root();
    let data = root.path().join("data");
    fs::DirBuilder::new().mode(0o700).create(&data).unwrap();
    let parent = Directory::owned(&data).unwrap();
    let mut stage = Staged::tracked(
        &parent,
        ".test-",
        root.path(),
        "birth".as_ref(),
        StagePurpose::Genesis,
        None,
        None,
    )
    .unwrap();
    let original = FileId::of(&stage.file.metadata().unwrap());
    stage.file.write_all(b"birth payload").unwrap();
    let mut retained = stage
        .publish_retained("birth".as_ref(), b"birth payload")
        .unwrap();
    assert_eq!(FileId::of(&retained.file.metadata().unwrap()), original);
    retained.file.try_lock_exclusive().unwrap();
    assert_eq!(
        File::open(data.join("birth"))
            .unwrap()
            .try_lock_exclusive()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    retained.file.seek(SeekFrom::Start(0)).unwrap();
    let mut payload = Vec::new();
    retained.file.read_to_end(&mut payload).unwrap();
    assert_eq!(payload, b"birth payload");
    FileExt::unlock(&retained.file).unwrap();
}

#[test]
fn successful_publication_releases_the_writer_lock_despite_a_duplicate_fd() {
    let root = root();
    let data = root.path().join("data");
    fs::DirBuilder::new().mode(0o700).create(&data).unwrap();
    let parent = Directory::owned(&data).unwrap();
    let mut stage = Staged::tracked(
        &parent,
        ".test-",
        root.path(),
        "published".as_ref(),
        StagePurpose::Metadata,
        None,
        None,
    )
    .unwrap();
    let inherited = stage.file.try_clone().unwrap();
    let original = FileId::of(&stage.file.metadata().unwrap());
    stage.file.write_all(b"whole payload").unwrap();
    stage
        .publish("published".as_ref(), b"whole payload", None)
        .unwrap();
    assert_eq!(identity(&data.join("published")), original);
    assert_eq!(fs::read(data.join("published")).unwrap(), b"whole payload");
    let next = File::open(data.join("published")).unwrap();
    next.try_lock_exclusive().unwrap();
    drop(inherited);
    // Closing an old duplicate cannot unlock a successor's independent lock.
    assert_eq!(
        File::open(data.join("published"))
            .unwrap()
            .try_lock_exclusive()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    FileExt::unlock(&next).unwrap();
}

#[test]
fn an_unpublished_retained_writer_stays_locked_until_its_last_fd_closes() {
    isolated_retention("unpublished");
}

fn unpublished_writer() {
    let root = root();
    let data = root.path().join("data");
    fs::DirBuilder::new().mode(0o700).create(&data).unwrap();
    let parent = Directory::owned(&data).unwrap();
    let mut stage = Staged::tracked(
        &parent,
        ".test-",
        root.path(),
        "published".as_ref(),
        StagePurpose::Metadata,
        None,
        None,
    )
    .unwrap();
    stage.file.write_all(b"pending payload").unwrap();
    stage.seal(b"pending payload").unwrap();
    let name = stage.name.clone();
    let inherited = stage.file.try_clone().unwrap();
    stage.keep_on_failure();
    drop(stage);
    let next = File::open(data.join(name)).unwrap();
    assert_eq!(
        next.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(inherited);
    next.try_lock_exclusive().unwrap();
    FileExt::unlock(&next).unwrap();
}

#[test]
fn a_parent_sync_failure_cannot_release_the_unconfirmed_writer_lock() {
    isolated_retention("sync-failure");
}

fn unconfirmed_writer() {
    let root = root();
    let data = root.path().join("data");
    fs::DirBuilder::new().mode(0o700).create(&data).unwrap();
    let parent = Directory::owned(&data).unwrap();
    let mut stage = Staged::tracked(
        &parent,
        ".test-",
        root.path(),
        "published".as_ref(),
        StagePurpose::Metadata,
        None,
        None,
    )
    .unwrap();
    stage.file.write_all(b"unconfirmed payload").unwrap();
    let inherited = stage.file.try_clone().unwrap();
    let refusal = crate::staging::tests::durability::Refusal::at(&data);
    assert!(
        stage
            .publish("published".as_ref(), b"unconfirmed payload", None)
            .is_err()
    );
    drop(refusal);
    let next = File::open(data.join("published")).unwrap();
    assert_eq!(
        next.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(inherited);
    next.try_lock_exclusive().unwrap();
    FileExt::unlock(&next).unwrap();
}

fn isolated_retention(case: &str) {
    // A last-FD assertion requires a process with a known complete FD set.
    // Other parallel tests fork; their pre-exec copies intentionally retain an
    // unpublished writer. Isolate only that semantic assertion, not the suite.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "claude::metadata::staged::tests::lock_lifetime::retained_writer_description_child",
            "--ignored",
            "--nocapture",
        ])
        .env("AGENTCHAT_RETAINED_FD_TEST_CASE", case)
        .env_remove("AGENTCHAT_CONTEXT")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!("retention verified: {case}"))
    );
}

#[test]
#[ignore = "owned subprocess fixture; parents verify the exact final descriptor boundary"]
fn retained_writer_description_child() {
    let case = std::env::var("AGENTCHAT_RETAINED_FD_TEST_CASE").unwrap();
    match case.as_str() {
        "unpublished" => unpublished_writer(),
        "sync-failure" => unconfirmed_writer(),
        _ => panic!("unknown retained writer case"),
    }
    println!("retention verified: {case}");
}
