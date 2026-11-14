//! Actual ordinary producer cuts; physical recovery never means host admission.
use crate::{
    claude::{
        namespace::{OwnerNamespace, OwnerReaper},
        observer::NativeProfile,
        process::{BirthState, ProcessIdentity},
    },
    staging::OriginReaper,
};
use std::{
    ffi::OsStr,
    fs,
    io::Write,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(in crate::claude::metadata::staged) fn cut(parent: &Path, stage: &OsStr, target: &OsStr) {
    let Some(wanted) = std::env::var_os("AGENTCHAT_PUBLISHER_CUT_TARGET") else {
        return;
    };
    let home = PathBuf::from(std::env::var_os("AGENTCHAT_HOME").unwrap());
    if target != wanted || !parent.starts_with(&home) {
        return;
    }
    let marker = PathBuf::from(std::env::var_os("AGENTCHAT_PUBLISHER_READY").unwrap());
    let m = fs::symlink_metadata(parent.join(stage)).unwrap();
    let value = serde_json::json!({"parent":parent,"stage":stage.to_str().unwrap(),
        "target":target.to_str().unwrap(),"device":m.dev(),"inode":m.ino(),"length":m.len()});
    let temporary = marker.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap();
    file.write_all(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    fs::rename(temporary, marker).unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn path(key: &str) -> PathBuf {
    std::env::var_os(key).unwrap().into()
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[tokio::test]
#[ignore = "owned ordinary publisher stopped at its original complete stage"]
async fn writer_child() {
    let home = path("AGENTCHAT_HOME");
    let mode = std::env::var("AGENTCHAT_PUBLISHER_MODE").unwrap();
    let owner: ProcessIdentity =
        serde_json::from_str(&std::env::var("AGENTCHAT_PUBLISHER_NATIVE").unwrap()).unwrap();
    if mode == "certificate" {
        assert_eq!(owner.birth_state(), BirthState::Gone);
        let mut reaper = OwnerReaper::new(&home);
        loop {
            let _ = reaper.step().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    } else if mode == "input" {
        let ns = OwnerNamespace::existing(&home, owner).await.unwrap();
        crate::claude::body::tests::input()
            .publish(&ns.metadata_publisher().unwrap(), ns.directory())
            .unwrap();
    } else if mode == "profile" {
        let ns = OwnerNamespace::existing(&home, owner.clone())
            .await
            .unwrap();
        ns.publish_profile(NativeProfile {
            executable: owner.executable(),
            version: "2.1.301".into(),
        })
        .await
        .unwrap();
    } else {
        // The lock and initialized-marker modes both enter the actual ordinary
        // first-owner path. The cut selects the required real publication.
        OwnerNamespace::open(&home, owner).await.unwrap();
    }
    panic!("Ordinary publisher missed its selected complete boundary");
}

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    owner: ProcessIdentity,
    native: Owned,
    marker: PathBuf,
    namespace: Option<OwnerNamespace>,
    mode: String,
}
impl Fixture {
    async fn new(mode: &str) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let root = tempfile::tempdir().unwrap();
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
        let owner = token.unwrap();
        let namespace = if matches!(mode, "profile" | "certificate" | "input") {
            Some(OwnerNamespace::open(&home, owner.clone()).await.unwrap())
        } else {
            None
        };
        Self {
            _root: root,
            home,
            owner,
            native,
            marker: base.join("ready.json"),
            namespace,
            mode: mode.into(),
        }
    }
    fn stop_native(&mut self) {
        self.native.0.kill().unwrap();
        assert_eq!(self.native.0.wait().unwrap().signal(), Some(9));
        assert_eq!(self.owner.birth_state(), BirthState::Gone);
    }
    fn spawn(&self) -> Owned {
        let target = match self.mode.as_str() {
            "profile" => "native-profile.json".to_owned(),
            "input" => crate::claude::body::tests::input()
                .path(self.namespace.as_ref().unwrap().directory())
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
            "lock" => "owner-state.lock".into(),
            "marker" => "owner-state.initialized.json".into(),
            "certificate" => format!(
                ".gc-{}.json",
                self.namespace
                    .as_ref()
                    .unwrap()
                    .directory()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
            ),
            _ => unreachable!(),
        };
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "claude::metadata::staged::tests::publishers::writer_child",
                    "--nocapture",
                ])
                .env("AGENTCHAT_HOME", &self.home)
                .env("AGENTCHAT_USER_CONFIG_ROOT", self.home.join("config"))
                .env_remove("AGENTCHAT_CONTEXT")
                .env("AGENTCHAT_PUBLISHER_CUT_TARGET", target)
                .env("AGENTCHAT_PUBLISHER_READY", &self.marker)
                .env("AGENTCHAT_PUBLISHER_MODE", &self.mode)
                .env(
                    "AGENTCHAT_PUBLISHER_NATIVE",
                    serde_json::to_string(&self.owner).unwrap(),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        wait(|| {
            assert!(child.0.try_wait().unwrap().is_none());
            self.marker.exists()
        });
        child
    }
    fn report(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(&self.marker).unwrap()).unwrap()
    }
    fn stage(&self) -> PathBuf {
        let r = self.report();
        PathBuf::from(r["parent"].as_str().unwrap()).join(r["stage"].as_str().unwrap())
    }
    fn kill_writer(&self, child: &mut Owned) {
        let creator = ProcessIdentity::capture(child.0.id()).unwrap();
        assert!(creator.is_current());
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        assert_eq!(creator.birth_state(), BirthState::Gone);
    }
    fn reap_stage(&self) {
        let stage = self.stage();
        let mut reaper = OriginReaper::new(&self.home);
        wait(|| {
            let page = reaper.step().unwrap();
            assert!(page.visited <= 8 && page.removed <= 16);
            !stage.exists()
        });
    }
}

#[tokio::test]
async fn an_actual_killed_profile_publisher_never_replays_its_future_native_profile() {
    let f = Fixture::new("profile").await;
    let ns = f.namespace.as_ref().unwrap();
    let header = fs::read(ns.directory().join("owner-state.json")).unwrap();
    let lock = fs::read(ns.directory().join("owner-state.lock")).unwrap();
    let mut child = f.spawn();
    let stage = f.stage();
    assert!(stage.exists());
    let mut live = OriginReaper::new(&f.home);
    assert_eq!(live.step().unwrap().removed, 0);
    f.kill_writer(&mut child);
    assert!(f.owner.is_current());
    f.reap_stage();
    assert!(!ns.directory().join("native-profile.json").exists());
    assert_eq!(
        fs::read(ns.directory().join("owner-state.json")).unwrap(),
        header
    );
    assert_eq!(
        fs::read(ns.directory().join("owner-state.lock")).unwrap(),
        lock
    );
    assert_eq!(ns.generation_floor().await.unwrap(), 0);
    ns.publish_profile(NativeProfile {
        executable: f.owner.executable(),
        version: "2.1.301".into(),
    })
    .await
    .unwrap();
    let value: NativeProfile =
        crate::claude::metadata::read(&ns.directory().join("native-profile.json")).unwrap();
    assert_eq!(value.executable, f.owner.executable());
}

#[tokio::test]
async fn an_actual_killed_full_input_publisher_discards_only_its_unpublished_stage() {
    let mut f = Fixture::new("input").await;
    let ns = f.namespace.clone().unwrap();
    let header = fs::read(ns.directory().join("owner-state.json")).unwrap();
    let input = crate::claude::body::tests::input();
    let mut child = f.spawn();
    assert!(fs::metadata(f.stage()).unwrap().len() > 393_216);
    let mut live = OriginReaper::new(&f.home);
    assert_eq!(live.step().unwrap().removed, 0);
    f.kill_writer(&mut child);
    assert!(f.owner.is_current());
    f.reap_stage();
    assert!(!input.path(ns.directory()).exists());
    assert_eq!(
        fs::read(ns.directory().join("owner-state.json")).unwrap(),
        header
    );
    assert_eq!(ns.generation_floor().await.unwrap(), 0);
    input
        .publish(&ns.metadata_publisher().unwrap(), ns.directory())
        .unwrap();
    assert_eq!(
        crate::claude::body::InputFile::read(&input.path(ns.directory())).unwrap(),
        input
    );
    // Published transport data is also bounded by actual original-owner loss.
    // This is physical collection only, not application retirement or G0 proof.
    let reservation = ns
        .reserve(input.root, "/test-owned/workspace".into(), None)
        .await
        .unwrap();
    assert_eq!(reservation.generation, input.native_generation);
    let publisher = ns.metadata_publisher().unwrap();
    let pending = crate::claude::body::PendingInput::new(&input, 1).unwrap();
    pending.publish(&publisher, ns.directory()).unwrap();
    crate::claude::body::InputHead::new(&input)
        .unwrap()
        .publish(&publisher, ns.directory())
        .unwrap();
    let receipt = crate::claude::body::InputReceipt::from_native(
        &input,
        crate::claude::body::NativeInputResult::Append(serde_json::json!({
            "uuid":uuid::Uuid::new_v4(),"message":{"type":"user","role":"user","isMeta":true,
                "content":[{"type":"text","text":input.text}]}
        })),
    )
    .unwrap();
    receipt.publish(&publisher, ns.directory()).unwrap();
    let mut live = OwnerReaper::new(&f.home);
    assert_eq!(live.step().unwrap().removed, 0);
    assert!(input.path(ns.directory()).exists());
    assert!(crate::claude::body::PendingInput::path(ns.directory(), input.context_id).exists());
    assert!(
        crate::claude::body::InputReceipt::path(ns.directory(), input.context_id, input.token)
            .exists()
    );
    f.stop_native();
    let mut reaper = OwnerReaper::new(&f.home);
    wait(|| {
        let page = reaper.step().unwrap();
        assert!(page.visited <= 8 && page.removed <= 40 && page.directories_removed <= 1);
        !ns.directory().exists()
    });
}

#[tokio::test]
async fn actual_killed_unpublished_genesis_lock_and_marker_preserve_pristine_retry() {
    for mode in ["lock", "marker"] {
        let f = Fixture::new(mode).await;
        let mut child = f.spawn();
        let report = f.report();
        let parent = PathBuf::from(report["parent"].as_str().unwrap());
        let stage = f.stage();
        let target = parent.join(report["target"].as_str().unwrap());
        assert!(!target.exists());
        let header = fs::read(parent.join("owner-state.json")).ok();
        let lock = fs::read(parent.join("owner-state.lock")).ok();
        f.kill_writer(&mut child);
        assert!(f.owner.is_current());
        f.reap_stage();
        assert!(!stage.exists() && !target.exists());
        assert_eq!(fs::read(parent.join("owner-state.json")).ok(), header);
        assert_eq!(fs::read(parent.join("owner-state.lock")).ok(), lock);
        let ns = OwnerNamespace::open(&f.home, f.owner.clone())
            .await
            .unwrap();
        assert_eq!(ns.directory(), parent);
        assert_eq!(ns.generation_floor().await.unwrap(), 0);
        assert_eq!(
            &fs::read(parent.join("owner-state.lock")).unwrap()[..2],
            b"1\n"
        );
        assert!(parent.join("owner-state.initialized.json").exists());
    }
}

#[tokio::test]
async fn an_actual_killed_certificate_publisher_keeps_original_quarantine_recoverable() {
    let mut f = Fixture::new("certificate").await;
    let ns = f.namespace.clone().unwrap();
    let original = fs::read(ns.directory().join("owner-state.json")).unwrap();
    f.stop_native();
    let mut child = f.spawn();
    let report = f.report();
    let parent = PathBuf::from(report["parent"].as_str().unwrap());
    let target = parent.join(report["target"].as_str().unwrap());
    assert!(!target.exists());
    let quarantine = parent.join(format!(
        ".retired-{}",
        ns.directory().file_name().unwrap().to_str().unwrap()
    ));
    assert_eq!(
        fs::read(quarantine.join("owner-state.json")).unwrap(),
        original
    );
    f.kill_writer(&mut child);
    f.reap_stage();
    assert!(!target.exists());
    assert_eq!(
        fs::read(quarantine.join("owner-state.json")).unwrap(),
        original
    );
    let mut collector = OwnerReaper::new(&f.home);
    wait(|| {
        let page = collector.step().unwrap();
        assert!(page.visited <= 8 && page.removed <= 40 && page.directories_removed <= 1);
        !quarantine.exists() && !target.exists()
    });
    assert!(!ns.directory().exists());
}
