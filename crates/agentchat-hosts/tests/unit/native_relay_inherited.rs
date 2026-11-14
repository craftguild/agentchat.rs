//! A real descendant keeps the original reply listener after its publisher dies.
use super::*;
use std::{
    os::unix::fs::OpenOptionsExt,
    process::{Command, Stdio},
};

const CHILD: &str =
    "claude::observer::relay::recovery::tests::inherited::inherited_publisher_child";

#[tokio::test]
#[ignore = "owned publisher deliberately passes its original listener to a child"]
async fn inherited_publisher_child() {
    let root = PathBuf::from(std::env::var_os("AGENTCHAT_RELAY_TEST_ROOT").unwrap());
    let owner =
        serde_json::from_str(&std::env::var("AGENTCHAT_RELAY_TEST_OWNER").unwrap()).unwrap();
    let publication = super::super::super::publication::bind(&root, "observer", &owner)
        .await
        .unwrap();
    // Only this test removes CLOEXEC. Product listeners never deliberately
    // escape; recovery must still preserve an unexpectedly inherited listener.
    rustix::io::fcntl_setfd(&publication.replies, rustix::io::FdFlags::empty()).unwrap();
    let mut child = Command::new("/bin/cat")
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let token = ProcessIdentity::capture(child.id()).unwrap();
    assert!(token.is_current());
    let report = root.join("inherited.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(report)
        .unwrap();
    file.write_all(&serde_json::to_vec(&token).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    // Parent sends SIGKILL after reading the complete publication. Keeping both
    // handles alive makes the exact original reply socket survive that death.
    loop {
        assert!(child.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct SocketDirectory(PathBuf, (u64, u64));
impl Drop for SocketDirectory {
    fn drop(&mut self) {
        if self.0.exists() && support::identity(&self.0) == self.1 {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[tokio::test]
async fn original_inherited_reply_listener_blocks_all_inactive_cleanup() {
    let root = support::private_root();
    let owner = ProcessIdentity::capture(std::process::id()).unwrap();
    let mut command = support::Fixture::command(CHILD, root.path(), &owner);
    command.stdin(Stdio::piped());
    let mut child = Owned(command.spawn().unwrap());
    // Child::wait closes its own stdin. Hold the write end independently so
    // reaping the killed publisher does not also terminate the inherited child.
    let input = child.0.stdin.take().unwrap();
    let report = root.path().join("inherited.json");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let inherited: ProcessIdentity = loop {
        if let Ok(bytes) = fs::read(&report)
            && let Ok(value) = serde_json::from_slice(&bytes)
        {
            break value;
        }
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(inherited.is_current());
    let endpoint = root.path().join("fresh-observer.json");
    let bytes = fs::read(&endpoint).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let directory = PathBuf::from(value["socket"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_owned();
    let _directory = SocketDirectory(directory.clone(), support::identity(&directory));
    let paths = [
        endpoint.clone(),
        directory.join("request.sock"),
        directory.join("reply.sock"),
    ];
    let before = paths
        .iter()
        .map(|p| support::identity(p))
        .collect::<Vec<_>>();
    let publisher: ProcessIdentity =
        serde_json::from_value(value["ownership"]["relay"].clone()).unwrap();
    assert_eq!(publisher.pid(), child.0.id());
    child.0.kill().unwrap();
    assert_eq!(child.0.wait().unwrap().signal(), Some(9));
    assert_eq!(publisher.birth_state(), BirthState::Gone);
    assert!(inherited.is_current());
    assert!(retire(&endpoint, "observer", &owner).await.is_err());
    assert_eq!(fs::read(&endpoint).unwrap(), bytes);
    for (path, id) in paths.iter().zip(before) {
        assert_eq!(support::identity(path), id);
    }
    // EOF makes our inherited child exit normally. No process-name kill or
    // forged birth observation is used to permit the subsequent real recovery.
    drop(input);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while inherited.birth_state() != BirthState::Gone {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    retire(&endpoint, "observer", &owner).await.unwrap();
    assert!(!endpoint.exists());
    assert!(!directory.exists());
}
