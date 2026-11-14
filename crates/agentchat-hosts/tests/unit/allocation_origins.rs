//! Real files/listeners and original physical loss, not native/model admission.
use super::*;
use crate::{claude::process::BirthState, staging::OriginReaper};
use std::{
    fs,
    io::Write,
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        net::UnixListener,
        process::ExitStatusExt,
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) fn cut(phase: &str, path: &Path) {
    if std::env::var("AGENTCHAT_ALLOCATION_CUT").ok().as_deref() != Some(phase)
        || (std::env::var_os("AGENTCHAT_ALLOCATION_PATH").as_deref() != Some(path.as_os_str())
            && std::env::var_os("AGENTCHAT_ALLOCATION_MANAGED").is_none())
    {
        return;
    }
    let ready = PathBuf::from(std::env::var_os("AGENTCHAT_ALLOCATION_READY").unwrap());
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(ready.with_extension("tmp"))
        .unwrap();
    file.write_all(&serde_json::to_vec(&serde_json::json!({"phase":phase,"path":path})).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    fs::rename(ready.with_extension("tmp"), ready).unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn env(name: &str) -> PathBuf {
    std::env::var_os(name).unwrap().into()
}
fn wait(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn normal_ready(path: &Path) -> ! {
    let ready = env("AGENTCHAT_ALLOCATION_READY");
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(ready.with_extension("tmp"))
        .unwrap();
    file.write_all(&serde_json::to_vec(&serde_json::json!({"path":path})).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    fs::rename(ready.with_extension("tmp"), &ready).unwrap();
    loop {
        std::thread::park();
    }
}
#[test]
#[ignore = "owned physical allocation writer or collector at its selected cut"]
fn allocation_child() {
    let home = env("AGENTCHAT_HOME");
    if let Ok(kind) = std::env::var("AGENTCHAT_ALLOCATION_MANAGED") {
        let native: ProcessIdentity =
            serde_json::from_str(&std::env::var("AGENTCHAT_ALLOCATION_NATIVE").unwrap()).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let namespace = crate::claude::namespace::OwnerNamespace::existing(&home, native)
                    .await
                    .unwrap();
                if kind == "gateway" {
                    let lease = crate::claude::namespace::TransportLease::bind_managed(&namespace)
                        .await
                        .unwrap();
                    if std::env::var("AGENTCHAT_ALLOCATION_CUT").ok().as_deref() == Some("normal") {
                        let path = lease
                            .listener
                            .local_addr()
                            .unwrap()
                            .as_pathname()
                            .unwrap()
                            .parent()
                            .unwrap()
                            .to_owned();
                        drop(lease);
                        normal_ready(&path);
                    }
                    std::hint::black_box(&lease);
                    panic!("Managed gateway missed cut");
                } else {
                    if std::env::var("AGENTCHAT_ALLOCATION_CUT").ok().as_deref() == Some("normal") {
                        let slot = namespace
                            .directory()
                            .join(format!("fresh-{}.json", namespace.observer()));
                        let task = tokio::spawn(crate::claude::observer::relay::serve_managed(
                            namespace,
                            tokio::io::sink(),
                        ));
                        let deadline = Instant::now() + Duration::from_secs(20);
                        while !slot.exists() {
                            assert!(Instant::now() < deadline);
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        let value: serde_json::Value =
                            serde_json::from_slice(&fs::read(&slot).unwrap()).unwrap();
                        let path = PathBuf::from(value["socket"].as_str().unwrap())
                            .parent()
                            .unwrap()
                            .to_owned();
                        task.abort();
                        assert!(task.await.unwrap_err().is_cancelled());
                        normal_ready(&path);
                    }
                    crate::claude::observer::relay::serve_managed(namespace, tokio::io::sink())
                        .await
                        .unwrap();
                }
            });
        panic!("Managed publisher missed cut");
    }
    let path = env("AGENTCHAT_ALLOCATION_PATH");
    if std::env::var_os("AGENTCHAT_ALLOCATION_COLLECTOR").is_some() {
        let mut reaper = OriginReaper::new(&home);
        loop {
            let _ = reaper.step().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let root = env("AGENTCHAT_ALLOCATION_ROOT");
    let mut journal = OriginRegistry::new(&home)
        .begin_allocation(AllocationPlan {
            path: &path,
            root: &root,
            endpoint: "gateway-00000000-0000-4000-8000-000000000001.json".as_ref(),
            alternate_root: None,
            sockets: &["control.sock"],
            guard: None,
            purpose: StagePurpose::Gateway,
            native: None,
        })
        .unwrap();
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    cut("directory-unproved", &path);
    journal.directory_created().unwrap();
    let listener = UnixListener::bind(path.join("control.sock")).unwrap();
    fs::set_permissions(path.join("control.sock"), fs::Permissions::from_mode(0o600)).unwrap();
    cut("socket-unproved", &path);
    let inherited = if std::env::var("AGENTCHAT_ALLOCATION_INHERIT")
        .ok()
        .as_deref()
        == Some("1")
    {
        rustix::io::fcntl_setfd(&listener, rustix::io::FdFlags::empty()).unwrap();
        let child = Command::new("/bin/cat")
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let expected = Path::new("/bin/cat").canonicalize().unwrap();
        let mut token = None;
        wait(|| {
            token = ProcessIdentity::capture(child.id())
                .ok()
                .filter(|t| t.executable() == expected && t.is_current());
            token.is_some()
        });
        fs::write(
            env("AGENTCHAT_ALLOCATION_READY").with_extension("inherited"),
            serde_json::to_vec(&token.unwrap()).unwrap(),
        )
        .unwrap();
        Some(child)
    } else {
        None
    };
    journal.socket_created("control.sock").unwrap();
    std::hint::black_box((&journal, &listener, &inherited));
    panic!("Allocation missed its selected cut");
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    path: PathBuf,
    _native: Option<Owned>,
    external: bool,
    input: Option<std::process::ChildStdin>,
    inherited: Option<ProcessIdentity>,
}
impl Fixture {
    fn managed(kind: &str, phase: &str) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("state");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let mut native = Owned(
            Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let expected = Path::new("/bin/cat").canonicalize().unwrap();
        let mut token = None;
        wait(|| {
            assert!(native.0.try_wait().unwrap().is_none());
            token = ProcessIdentity::capture(native.0.id())
                .ok()
                .filter(|t| t.executable() == expected && t.is_current());
            token.is_some()
        });
        let token = token.unwrap();
        let ns = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(crate::claude::namespace::OwnerNamespace::open(
                &home,
                token.clone(),
            ))
            .unwrap();
        let endpoint = ns.directory().to_owned();
        let ready = base.join("ready");
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "staging::allocation::tests::allocation_child",
                    "--nocapture",
                ])
                .env("AGENTCHAT_HOME", &home)
                .env("AGENTCHAT_USER_CONFIG_ROOT", base.join("config"))
                .env_remove("AGENTCHAT_CONTEXT")
                .env_remove("AGENTCHAT_ALLOCATION_PATH")
                .env("AGENTCHAT_ALLOCATION_MANAGED", kind)
                .env(
                    "AGENTCHAT_ALLOCATION_NATIVE",
                    serde_json::to_string(&token).unwrap(),
                )
                .env("AGENTCHAT_ALLOCATION_CUT", phase)
                .env("AGENTCHAT_ALLOCATION_READY", &ready)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        wait(|| {
            assert!(child.0.try_wait().unwrap().is_none());
            ready.exists()
        });
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&ready).unwrap()).unwrap();
        let path = PathBuf::from(value["path"].as_str().unwrap());
        if phase == "normal" {
            assert!(!path.exists());
            assert!(fs::read_dir(home.join("staging")).unwrap().next().is_none());
        }
        let creator = ProcessIdentity::capture(child.0.id()).unwrap();
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        assert_eq!(creator.birth_state(), BirthState::Gone);
        assert!(token.is_current());
        Self {
            _root: root,
            home,
            root: endpoint,
            path,
            _native: Some(native),
            external: true,
            input: None,
            inherited: None,
        }
    }
    fn at(phase: &str) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("state");
        let endpoint = base.join("root");
        for p in [&home, &endpoint] {
            fs::DirBuilder::new().mode(0o700).create(p).unwrap();
        }
        let path = base.join(format!("allocation-{}", Uuid::new_v4()));
        let ready = base.join("ready");
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "staging::allocation::tests::allocation_child",
                    "--nocapture",
                ])
                .env("AGENTCHAT_HOME", &home)
                .env("AGENTCHAT_USER_CONFIG_ROOT", base.join("config"))
                .env_remove("AGENTCHAT_CONTEXT")
                .env("AGENTCHAT_ALLOCATION_ROOT", &endpoint)
                .env("AGENTCHAT_ALLOCATION_PATH", &path)
                .env(
                    "AGENTCHAT_ALLOCATION_CUT",
                    if phase == "inherited" {
                        "socket-captured"
                    } else {
                        phase
                    },
                )
                .env(
                    "AGENTCHAT_ALLOCATION_INHERIT",
                    if phase == "inherited" { "1" } else { "0" },
                )
                .env("AGENTCHAT_ALLOCATION_READY", &ready)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let input = child.0.stdin.take();
        wait(|| {
            assert!(child.0.try_wait().unwrap().is_none());
            ready.exists()
        });
        let creator = ProcessIdentity::capture(child.0.id()).unwrap();
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        assert_eq!(creator.birth_state(), BirthState::Gone);
        let inherited = if phase == "inherited" {
            Some(
                serde_json::from_slice(&fs::read(ready.with_extension("inherited")).unwrap())
                    .unwrap(),
            )
        } else {
            None
        };
        Self {
            _root: root,
            home,
            root: endpoint,
            path,
            _native: None,
            external: false,
            input,
            inherited,
        }
    }
    fn collect(&self) {
        let mut reaper = OriginReaper::new(&self.home);
        wait(|| {
            let page = reaper.step().unwrap();
            assert!(page.visited <= 8 && page.removed <= 16 && page.directories_removed <= 1);
            fs::read_dir(self.home.join("staging"))
                .unwrap()
                .next()
                .is_none()
        });
    }
    fn preserve(&self) {
        let journal = self
            .home
            .join("staging")
            .join(format!("{}.json", key(&self.path).unwrap()));
        let original = fs::read(&journal).unwrap();
        let before = self.data_snapshot();
        let mut reaper = OriginReaper::new(&self.home);
        for _ in 0..5 {
            let page = reaper.step().unwrap();
            // An unrelated, completed metadata-stage witness may retire. The
            // original allocation journal and all its data must remain intact.
            assert!(page.removed <= 16 && page.visited <= 8);
            assert_eq!(page.directories_removed, 0);
            assert_eq!(fs::read(&journal).unwrap(), original);
            assert_eq!(self.data_snapshot(), before);
        }
        assert!(
            fs::read_dir(self.home.join("staging"))
                .unwrap()
                .next()
                .is_some()
        );
    }
    fn data_snapshot(&self) -> Vec<(std::ffi::OsString, u64, u64, u32, u64)> {
        use std::os::unix::fs::MetadataExt;
        let mut rows = fs::read_dir(&self.path)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let m = fs::symlink_metadata(e.path()).unwrap();
                (e.file_name(), m.dev(), m.ino(), m.mode(), m.len())
            })
            .collect::<Vec<_>>();
        rows.sort();
        rows
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.input.take());
        if let Some(token) = &self.inherited {
            let end = Instant::now() + Duration::from_secs(2);
            while token.birth_state() != BirthState::Gone && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if self.external {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
#[test]
fn a_live_inherited_listener_preserves_its_dead_creators_allocation() {
    let mut f = Fixture::at("inherited");
    let token = f.inherited.as_ref().unwrap().clone();
    assert_eq!(token.birth_state(), BirthState::Present);
    f.preserve();
    assert!(f.path.join("control.sock").exists());
    drop(f.input.take());
    wait(|| token.birth_state() == BirthState::Gone);
    f.collect();
    assert!(!f.path.exists());
}
#[test]
fn actual_managed_normal_drop_releases_every_original_allocation_without_waiting_for_death() {
    for kind in ["gateway", "relay"] {
        let f = Fixture::managed(kind, "normal");
        assert!(!f.path.exists());
        f.collect();
    }
}
#[test]
fn killed_allocation_collectors_resume_after_socket_or_directory_removal() {
    for phase in ["socket", "directory"] {
        let f = Fixture::at("socket-captured");
        let ready = f.home.with_extension("collector-ready");
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "staging::allocation::tests::allocation_child",
                    "--nocapture",
                ])
                .env("AGENTCHAT_HOME", &f.home)
                .env("AGENTCHAT_USER_CONFIG_ROOT", f.home.join("config"))
                .env_remove("AGENTCHAT_CONTEXT")
                .env("AGENTCHAT_ALLOCATION_PATH", &f.path)
                .env("AGENTCHAT_ALLOCATION_CUT", phase)
                .env("AGENTCHAT_ALLOCATION_READY", &ready)
                .env("AGENTCHAT_ALLOCATION_COLLECTOR", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        wait(|| {
            assert!(child.0.try_wait().unwrap().is_none());
            ready.exists()
        });
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        f.collect();
        assert!(!f.path.exists());
    }
}
#[test]
fn foreign_edits_or_aliases_of_published_managed_endpoints_preserve_the_whole_allocation() {
    for kind in ["gateway", "relay"] {
        for mode in ["edit", "alias"] {
            let f = Fixture::managed(kind, "published");
            let slot = fs::read_dir(&f.root)
                .unwrap()
                .map(|p| p.unwrap().path())
                .find(|p| {
                    p.file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with(if kind == "gateway" {
                            "gateway-"
                        } else {
                            "fresh-"
                        })
                        && p.extension() == Some("json".as_ref())
                })
                .unwrap();
            if mode == "edit" {
                fs::write(&slot, b"changed").unwrap();
            } else {
                fs::hard_link(&slot, slot.with_extension("alias")).unwrap();
            }
            f.preserve();
            assert!(f.path.exists());
        }
    }
}
#[test]
fn actual_managed_gateway_and_relay_cuts_recover_without_mutating_native_history() {
    for kind in ["gateway", "relay"] {
        for phase in [
            "intent",
            "directory-captured",
            "socket-captured",
            "endpoint-written",
            "published",
            "journal-stage",
        ] {
            let f = Fixture::managed(kind, phase);
            let header = fs::read(f.root.join("owner-state.json")).unwrap();
            let lock = fs::read(f.root.join("owner-state.lock")).unwrap();
            f.collect();
            assert!(!f.path.exists());
            assert_eq!(fs::read(f.root.join("owner-state.json")).unwrap(), header);
            assert_eq!(fs::read(f.root.join("owner-state.lock")).unwrap(), lock);
            assert!(!fs::read_dir(&f.root).unwrap().any(|p| {
                let name = p.unwrap().file_name();
                let s = name.to_str().unwrap();
                s.starts_with("fresh-") || s.starts_with("gateway-") && s.ends_with(".json")
            }));
        }
    }
}
#[test]
fn dead_intents_empty_directories_sockets_and_complete_checkpoint_swaps_recover() {
    for phase in [
        "intent",
        "directory-captured",
        "socket-captured",
        "journal-stage",
    ] {
        let f = Fixture::at(phase);
        f.collect();
        assert!(!f.path.exists());
        assert!(fs::read_dir(&f.root).unwrap().next().is_none());
    }
}
#[test]
fn unproved_creation_windows_and_foreign_children_or_replacements_are_preserved() {
    for mode in [
        "directory-unproved",
        "socket-unproved",
        "foreign",
        "replacement",
        "endpoint",
    ] {
        let f = Fixture::at(if mode.ends_with("unproved") {
            mode
        } else {
            "socket-captured"
        });
        match mode {
            "foreign" => fs::write(f.path.join("foreign"), b"keep").unwrap(),
            "replacement" => {
                fs::rename(&f.path, f.path.with_extension("original")).unwrap();
                fs::DirBuilder::new().mode(0o700).create(&f.path).unwrap();
            }
            "endpoint" => fs::write(
                f.root
                    .join("gateway-00000000-0000-4000-8000-000000000001.json"),
                b"foreign",
            )
            .unwrap(),
            _ => {}
        }
        f.preserve();
        assert!(f.path.exists());
    }
}

#[test]
fn a_live_managed_successor_keeps_its_endpoint_while_old_release_journals_retire() {
    struct External(PathBuf);
    impl Drop for External {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    for kind in ["gateway", "relay"] {
        let f = Fixture::managed(kind, "published");
        let old = f
            .home
            .join("staging")
            .join(format!("{}.json", key(&f.path).unwrap()));
        let native = ProcessIdentity::capture(f._native.as_ref().unwrap().0.id()).unwrap();
        let ready = f.home.with_extension("successor-ready");
        let mut next = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "staging::allocation::tests::allocation_child",
                    "--nocapture",
                ])
                .env("AGENTCHAT_HOME", &f.home)
                .env(
                    "AGENTCHAT_USER_CONFIG_ROOT",
                    f.home.join("successor-config"),
                )
                .env_remove("AGENTCHAT_CONTEXT")
                .env_remove("AGENTCHAT_ALLOCATION_PATH")
                .env("AGENTCHAT_ALLOCATION_MANAGED", kind)
                .env(
                    "AGENTCHAT_ALLOCATION_NATIVE",
                    serde_json::to_string(&native).unwrap(),
                )
                .env("AGENTCHAT_ALLOCATION_CUT", "published")
                .env("AGENTCHAT_ALLOCATION_READY", &ready)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        wait(|| {
            assert!(next.0.try_wait().unwrap().is_none());
            ready.exists()
        });
        let value: serde_json::Value = serde_json::from_slice(&fs::read(ready).unwrap()).unwrap();
        let new_path = External(value["path"].as_str().unwrap().into());
        let slot =
            fs::read_dir(&f.root)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    p.extension() == Some("json".as_ref())
                        && p.file_name().unwrap().to_str().unwrap().starts_with(
                            if kind == "gateway" {
                                "gateway-"
                            } else {
                                "fresh-"
                            },
                        )
                })
                .unwrap();
        let published = fs::read(&slot).unwrap();
        let mut reaper = OriginReaper::new(&f.home);
        for _ in 0..6 {
            let page = reaper.step().unwrap();
            assert!(page.visited <= 8 && page.removed <= 16 && page.directories_removed <= 1);
        }
        assert!(
            !old.exists(),
            "an active successor must not pin its predecessor's completed release journal"
        );
        assert!(new_path.0.exists());
        assert_eq!(fs::read(&slot).unwrap(), published);
        assert!(next.0.try_wait().unwrap().is_none());
        next.0.kill().unwrap();
        next.0.wait().unwrap();
        f.collect();
        assert!(!new_path.0.exists());
    }
}
