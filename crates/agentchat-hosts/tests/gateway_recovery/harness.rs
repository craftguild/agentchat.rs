//! Disposable physical processes and socket directories owned only by these tests.
use crate::claude::{
    metadata,
    namespace::{TransportEndpoint, TransportLease},
    process::{BirthState, ProcessIdentity},
};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::MetadataExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub(crate) struct OwnedChild(pub Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
pub(crate) fn identity(path: &Path) -> (u64, u64) {
    let m = fs::symlink_metadata(path).unwrap();
    (m.dev(), m.ino())
}
pub(crate) struct OwnedSocketDirectory(pub PathBuf, pub (u64, u64));
impl Drop for OwnedSocketDirectory {
    fn drop(&mut self) {
        // Every entry was authored by this fixture. Product recovery cannot
        // recursively remove a directory that may contain another writer's data.
        if self.0.exists() && identity(&self.0) == self.1 {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
pub(crate) struct Gateway {
    pub child: OwnedChild,
    pub helper: ProcessIdentity,
    pub endpoint: TransportEndpoint,
    pub inherited: Option<ProcessIdentity>,
    _directory: OwnedSocketDirectory,
}
impl Gateway {
    pub fn spawn(root: &Path, scope: Uuid, native_pid: u32, ready: &Path) -> Self {
        Self::spawn_mode(root, scope, native_pid, ready, "normal")
    }
    pub fn spawn_mode(root: &Path, scope: Uuid, native_pid: u32, ready: &Path, mode: &str) -> Self {
        // Rust's test filter omits the crate name. This works in both the
        // integration binary and a genuine private unit-test module.
        let module = module_path!().split_once("::").unwrap().1;
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    &format!("{module}::gateway_process_child"),
                    "--nocapture",
                ])
                .env("AGENTCHAT_GATEWAY_TEST_ROOT", root)
                .env("AGENTCHAT_GATEWAY_TEST_SCOPE", scope.to_string())
                .env("AGENTCHAT_GATEWAY_TEST_NATIVE_PID", native_pid.to_string())
                .env("AGENTCHAT_GATEWAY_TEST_READY", ready)
                .env("AGENTCHAT_GATEWAY_TEST_MODE", mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready.exists() {
            assert!(child.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "owned gateway readiness");
            std::thread::sleep(Duration::from_millis(5));
        }
        let helper = ProcessIdentity::capture(child.0.id()).unwrap();
        assert!(helper.is_current());
        let endpoint: TransportEndpoint =
            metadata::read(&root.join(format!("gateway-{scope}.json"))).unwrap();
        let inherited = serde_json::from_slice(&fs::read(ready).unwrap()).unwrap();
        let directory = endpoint.socket.parent().unwrap().to_owned();
        let id = identity(&directory);
        Self {
            child,
            helper,
            endpoint,
            inherited,
            _directory: OwnedSocketDirectory(directory, id),
        }
    }
    pub fn kill(&mut self) {
        self.child.0.kill().unwrap();
        assert_eq!(self.child.0.wait().unwrap().signal(), Some(9));
        assert_eq!(self.helper.birth_state(), BirthState::Gone);
        assert!(self.endpoint.socket.exists());
    }
    pub fn exec(&mut self) {
        self.child
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"X")
            .unwrap();
        let expected = Path::new("/bin/sleep").canonicalize().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(token) = ProcessIdentity::capture(self.helper.pid())
                && token.executable().canonicalize().unwrap() == expected
            {
                assert_ne!(token, self.helper);
                assert!(!self.helper.is_current());
                assert_eq!(self.helper.birth_state(), BirthState::Present);
                return;
            }
            assert!(self.child.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "owned helper exec");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[tokio::test]
#[ignore = "owned physical gateway subprocess; parents exercise actual exit and inheritance"]
async fn gateway_process_child() {
    let root = PathBuf::from(std::env::var_os("AGENTCHAT_GATEWAY_TEST_ROOT").unwrap());
    let scope = std::env::var("AGENTCHAT_GATEWAY_TEST_SCOPE")
        .unwrap()
        .parse()
        .unwrap();
    let native_pid = std::env::var("AGENTCHAT_GATEWAY_TEST_NATIVE_PID")
        .unwrap()
        .parse()
        .unwrap();
    let native = ProcessIdentity::capture(native_pid).unwrap();
    let lease = TransportLease::bind(&root, scope, &native).await.unwrap();
    let mut child = None;
    let inherited = if std::env::var("AGENTCHAT_GATEWAY_TEST_MODE").unwrap() == "inherit" {
        // Fixture-only CLOEXEC removal deliberately makes this actual original
        // listener survive helper SIGKILL. No product listener does this.
        rustix::io::fcntl_setfd(&*lease.listener, rustix::io::FdFlags::empty()).unwrap();
        let mut process = Command::new("/bin/cat")
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let expected = Path::new("/bin/cat").canonicalize().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let token = loop {
            if let Ok(token) = ProcessIdentity::capture(process.id())
                && token.executable().canonicalize().unwrap() == expected
                && token.is_current()
            {
                break token;
            }
            assert!(process.try_wait().unwrap().is_none());
            assert!(
                Instant::now() < deadline,
                "inherited listener image readiness"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        child = Some(OwnedChild(process));
        Some(token)
    } else {
        None
    };
    let ready = PathBuf::from(std::env::var_os("AGENTCHAT_GATEWAY_TEST_READY").unwrap());
    let stage = ready.with_extension("stage");
    fs::write(&stage, serde_json::to_vec(&inherited).unwrap()).unwrap();
    fs::rename(stage, ready).unwrap();
    let mut input = [0u8; 1];
    if std::io::stdin().read_exact(&mut input).is_ok() && input[0] == b'X' {
        let error = Command::new("/bin/sleep").arg("60").exec();
        panic!("owned exec failed: {error}");
    }
    drop(child);
    drop(lease);
}
