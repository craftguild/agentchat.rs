use agentchat_core::model::{Context, HostKind};
use agentchat_hosts::{
    claude::process::ProcessIdentity,
    launch::{Binding, Endpoint, LaunchOwners},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Child, Command},
    time::{Duration, Instant},
};
use uuid::Uuid;
pub struct Fixture {
    pub home: tempfile::TempDir,
    pub child: Child,
    pub binding: Binding,
}
impl Fixture {
    pub fn new() -> Self {
        let home = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let native = fs::canonicalize("/bin/sleep").unwrap();
        let meta = native.metadata().unwrap();
        let child = Command::new(&native).arg("60").spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ProcessIdentity::capture(child.id())
            .is_ok_and(|p| p.matches_executable(&native, meta.clone()) && p.is_current())
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let binding = Binding::new(
            Context {
                context_id: Uuid::new_v4(),
                incarnation: Uuid::new_v4(),
                project_id: Uuid::new_v4(),
                host: HostKind::Claude,
                host_session_id: Uuid::new_v4().to_string(),
                host_version: "synthetic-readiness-test".into(),
                os_profile: "explicit-kernel-fixture".into(),
                working_directory: home.path().canonicalize().unwrap(),
            },
            LaunchOwners::capture(child.id(), std::process::id()).unwrap(),
            Endpoint::Claude,
        )
        .unwrap();
        Self {
            home,
            child,
            binding,
        }
    }
    pub fn stop(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
