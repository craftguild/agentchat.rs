//! Owned real relay processes and private test-only control sockets.
use super::{BirthState, ProcessIdentity, relay};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    time::{Instant, timeout},
};

pub fn private_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("agentchat-relay-case-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap()
}
pub fn identity(path: &Path) -> (u64, u64) {
    let m = fs::symlink_metadata(path).unwrap();
    (m.dev(), m.ino())
}
#[tokio::test]
#[ignore = "owned subprocess; parents exercise actual relay SIGKILL and recovery"]
async fn relay_process_child() {
    let root = PathBuf::from(std::env::var_os("AGENTCHAT_RELAY_TEST_ROOT").unwrap());
    let owner: ProcessIdentity =
        serde_json::from_str(&std::env::var("AGENTCHAT_RELAY_TEST_OWNER").unwrap()).unwrap();
    if std::env::var("AGENTCHAT_RELAY_TEST_ACTION").as_deref() == Ok("retire") {
        relay::retire(&root, "observer", &owner).await.unwrap();
        return;
    }
    let control = UnixListener::bind(root.join("test-control.sock")).unwrap();
    fs::set_permissions(
        root.join("test-control.sock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let path = root.clone();
    let mut task =
        tokio::spawn(
            async move { relay::serve(&path, "observer", owner, tokio::io::sink()).await },
        );
    tokio::select! {
        result = &mut task => result.unwrap().unwrap(),
        result = control.accept() => {
            let (mut peer, _) = result.unwrap();
            let mut byte = [0]; peer.read_exact(&mut byte).await.unwrap();
            assert_eq!(byte, [b's']);
            task.abort(); assert!(task.await.unwrap_err().is_cancelled());
        }
    }
}

pub struct Fixture {
    pub root: tempfile::TempDir,
    pub owner: ProcessIdentity,
    pub endpoint: Value,
    pub directory: PathBuf,
    child: Option<Child>,
    directories: Vec<(PathBuf, (u64, u64))>,
}
impl Fixture {
    pub async fn new(entry: &str) -> Self {
        Self::with_owner(entry, ProcessIdentity::capture(std::process::id()).unwrap()).await
    }
    pub async fn with_owner(entry: &str, owner: ProcessIdentity) -> Self {
        let root = private_root();
        let child = Self::command(entry, root.path(), &owner).spawn().unwrap();
        let mut value = Self {
            root,
            owner,
            endpoint: Value::Null,
            directory: PathBuf::new(),
            child: Some(child),
            directories: Vec::new(),
        };
        value.ready().await;
        value
    }
    async fn ready(&mut self) {
        let path = self.endpoint_path();
        let deadline = Instant::now() + Duration::from_secs(20);
        let endpoint: Value = loop {
            if let Ok(bytes) = fs::read(&path)
                && let Ok(value) = serde_json::from_slice(&bytes)
            {
                break value;
            }
            assert!(
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "Owned publisher exited before ready"
            );
            assert!(Instant::now() < deadline, "Owned publisher did not publish");
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let relay: ProcessIdentity =
            serde_json::from_value(endpoint["ownership"]["relay"].clone()).unwrap();
        assert_eq!(relay.pid(), self.child.as_ref().unwrap().id());
        assert!(relay.is_current());
        let socket = Path::new(endpoint["socket"].as_str().unwrap());
        // The malformed request closes normally and proves this exact server
        // entered its request loop; it emits no native question or model turn.
        timeout(Duration::from_secs(5), async {
            let mut stream = UnixStream::connect(socket).await.unwrap();
            stream.write_all(b"{}\n").await.unwrap();
            stream.shutdown().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert!(bytes.is_empty());
        })
        .await
        .unwrap();
        let directory = socket.parent().unwrap().to_owned();
        self.directories
            .push((directory.clone(), identity(&directory)));
        self.directory = directory;
        self.endpoint = endpoint;
    }
    pub async fn restart(&mut self, entry: &str) {
        assert!(self.child.is_none());
        fs::remove_file(self.root.path().join("test-control.sock")).unwrap();
        self.child = Some(
            Self::command(entry, self.root.path(), &self.owner)
                .spawn()
                .unwrap(),
        );
        self.ready().await;
    }
    pub fn command(entry: &str, root: &Path, owner: &ProcessIdentity) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", entry, "--nocapture"])
            .env("AGENTCHAT_RELAY_TEST_ROOT", root)
            .env(
                "AGENTCHAT_RELAY_TEST_OWNER",
                serde_json::to_string(owner).unwrap(),
            )
            .env("AGENTCHAT_HOME", root.join("state"))
            .env("AGENTCHAT_USER_CONFIG_ROOT", root.join("config"))
            .env_remove("AGENTCHAT_CONTEXT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        command
    }
    pub fn endpoint_path(&self) -> PathBuf {
        self.root.path().join("fresh-observer.json")
    }
    pub fn remember_directory(&mut self, path: &Path) {
        self.directories.push((path.into(), identity(path)));
    }
    pub fn crash(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().unwrap().signal(), Some(9));
        let relay: ProcessIdentity =
            serde_json::from_value(self.endpoint["ownership"]["relay"].clone()).unwrap();
        assert_eq!(relay.birth_state(), BirthState::Gone);
    }
    pub async fn finish(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < deadline, "Owned relay did not stop");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            self.child.take();
        }
    }
    pub async fn stop(&mut self) {
        let mut stream = UnixStream::connect(self.root.path().join("test-control.sock"))
            .await
            .unwrap();
        stream.write_all(b"s").await.unwrap();
        self.finish().await;
    }
    pub fn snapshot(&self) -> Vec<(PathBuf, (u64, u64))> {
        [
            self.endpoint_path(),
            self.directory.join("request.sock"),
            self.directory.join("reply.sock"),
        ]
        .into_iter()
        .filter(|p| p.exists())
        .map(|p| {
            let id = identity(&p);
            (p, id)
        })
        .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        // These directories were created by this owned child or explicitly by
        // its parent fixture. Never remove an unrecorded or substituted path.
        for (path, id) in self.directories.iter().rev() {
            if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && (m.dev(), m.ino()) == *id) {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
}

pub struct NativeOwner {
    child: Child,
    _root: tempfile::TempDir,
    pub token: ProcessIdentity,
}
#[test]
#[ignore = "owned native process; parent controls real exec and normal exit"]
fn native_owner_child() {
    use std::io::BufRead;
    let ready = PathBuf::from(std::env::var_os("AGENTCHAT_RELAY_NATIVE_READY").unwrap());
    fs::write(ready, b"native main ready").unwrap();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    if line.trim() == "exec" {
        use std::os::unix::process::CommandExt;
        panic!("exec failed: {}", Command::new("/bin/cat").exec());
    }
    assert_eq!(line.trim(), "exit");
}
impl NativeOwner {
    pub async fn new(entry: &str) -> Self {
        let root = private_root();
        let ready = root.path().join("ready");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", entry, "--nocapture"])
            .env("AGENTCHAT_RELAY_NATIVE_READY", &ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut value = Self {
            child,
            _root: root,
            token: ProcessIdentity::capture(std::process::id()).unwrap(),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let token = loop {
            assert!(value.child.try_wait().unwrap().is_none());
            if ready.exists()
                && let Ok(token) = ProcessIdentity::capture(value.child.id())
                && token.is_current()
            {
                break token;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        value.token = token;
        value
    }
    pub async fn exec(&mut self) -> ProcessIdentity {
        use std::io::Write;
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"exec\n")
            .unwrap();
        let expected = Path::new("/bin/cat").canonicalize().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(token) = ProcessIdentity::capture(self.child.id())
                && token.executable().canonicalize().unwrap() == expected
                && token.is_current()
            {
                return token;
            }
            assert!(self.child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    pub fn exit(&mut self) {
        use std::io::Write;
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"exit\n")
            .unwrap();
        assert!(self.child.wait().unwrap().success());
        assert_eq!(self.token.birth_state(), BirthState::Gone);
    }
}
impl Drop for NativeOwner {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
