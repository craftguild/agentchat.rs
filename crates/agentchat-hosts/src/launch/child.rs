//! Actual supervised native child; executable birth is not host readiness.
use super::{LaunchOwners, native_executable};
use crate::claude::process::ProcessIdentity;
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{LaunchIdentity, LaunchOwnerState},
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::Path,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::process::{Child, Command};

/// Native terminal routing, independent of model/trust/approval controls.
#[derive(Clone, Copy, Default)]
pub enum NativeTerminal {
    /// Retain ordinary inherited native stdin/stdout/stderr.
    #[default]
    Inherited,
    /// Use the controlling terminal so the supervisor's stdout can carry JSON.
    Controlling,
}

/// A native process spawned by this supervisor, retaining its complete original
/// kernel identity. No constructor accepts an arbitrary PID. A separate exact
/// native root/trust/integration handshake must precede binding or membership.
pub struct NativeChild {
    child: Child,
    owner: ProcessIdentity,
    identity: LaunchIdentity,
    startup_root: Option<uuid::Uuid>,
}
/// Cloneable observation proof of a child actually spawned by this supervisor.
/// It cannot keep the process alive or grant a root, context or participation.
#[derive(Clone)]
pub struct NativeProof {
    owner: ProcessIdentity,
    identity: LaunchIdentity,
    startup_root: Option<uuid::Uuid>,
}
impl NativeProof {
    /// Launcher-selected native root; live native callbacks must confirm it.
    pub fn startup_root(&self) -> Option<uuid::Uuid> {
        self.startup_root
    }
    /// Original complete actual native image/birth identity.
    pub fn owner(&self) -> &ProcessIdentity {
        &self.owner
    }
    /// Original supervisor/child proof and launch generation.
    pub fn identity(&self) -> &LaunchIdentity {
        &self.identity
    }
    /// Fresh kernel-only check; retaining a clone cannot renew authority.
    pub fn state(&self) -> LaunchOwnerState {
        LaunchOwners::observe(&self.identity)
    }
}
impl NativeChild {
    /// Retain observation facts while concurrently polling the owned Child.
    /// The proof has no process ownership, signal or lifetime-extension ability.
    pub fn proof(&self) -> NativeProof {
        NativeProof {
            owner: self.owner.clone(),
            identity: self.identity.clone(),
            startup_root: self.startup_root,
        }
    }
    /// Enable the Claude first-input barrier before the native image can run.
    /// Environment UUIDs only select a pending barrier; neither proves a native
    /// conversation or releases input. The later original-child marker follows
    /// independently verified physical readiness, without joining. Ordinary host controls stay inherited.
    pub async fn spawn_with_startup_gate(
        executable: &Path,
        directory: &Path,
        arguments: &[OsString],
        environment: &BTreeMap<OsString, OsString>,
        root: uuid::Uuid,
    ) -> Result<Self> {
        Self::spawn_with_startup_gate_terminal(
            executable,
            directory,
            arguments,
            environment,
            root,
            NativeTerminal::Inherited,
        )
        .await
    }
    /// Managed startup with explicit native terminal routing. Root selection and
    /// actual-child capture remain identical; routing cannot grant readiness.
    pub async fn spawn_with_startup_gate_terminal(
        executable: &Path,
        directory: &Path,
        arguments: &[OsString],
        environment: &BTreeMap<OsString, OsString>,
        root: uuid::Uuid,
        terminal: NativeTerminal,
    ) -> Result<Self> {
        if root.is_nil() {
            return Err(unavailable());
        }
        let generation = uuid::Uuid::new_v4();
        let mut environment = environment.clone();
        environment.insert(super::startup::ROOT_ENV.into(), root.to_string().into());
        environment.insert(
            super::startup::GENERATION_ENV.into(),
            generation.to_string().into(),
        );
        let mut child =
            Self::spawn_terminal(executable, directory, arguments, &environment, terminal).await?;
        child.identity.generation = generation;
        child.startup_root = Some(root);
        Ok(child)
    }
    /// Start an actual Mach-O/ELF executable with inherited terminal and host
    /// policy. Arguments are supplied by trusted host composition, not forwarded
    /// arbitrary launch flags. Explicit HOME/CODEX_HOME/AGENTCHAT_CONTEXT overrides
    /// are refused, and an inherited AGENTCHAT_CONTEXT is removed for the fresh actor.
    /// Cancellation/error drops terminate only this still-owned startup child.
    /// Image capture is bounded separately from any later trust/ready wait.
    pub async fn spawn(
        executable: &Path,
        directory: &Path,
        arguments: &[OsString],
        environment: &BTreeMap<OsString, OsString>,
    ) -> Result<Self> {
        Self::spawn_terminal(
            executable,
            directory,
            arguments,
            environment,
            NativeTerminal::Inherited,
        )
        .await
    }
    /// Start trusted native composition with explicit terminal routing. This
    /// supplies no ready/root proof; the caller still verifies the exact native
    /// handshake before binding or membership. Host policy stays inherited.
    pub async fn spawn_with_terminal(
        executable: &Path,
        directory: &Path,
        arguments: &[OsString],
        environment: &BTreeMap<OsString, OsString>,
        terminal: NativeTerminal,
    ) -> Result<Self> {
        Self::spawn_terminal(executable, directory, arguments, environment, terminal).await
    }
    async fn spawn_terminal(
        executable: &Path,
        directory: &Path,
        arguments: &[OsString],
        environment: &BTreeMap<OsString, OsString>,
        terminal: NativeTerminal,
    ) -> Result<Self> {
        if !directory.is_absolute()
            || environment
                .keys()
                .any(|key| key == "HOME" || key == "CODEX_HOME" || key == "AGENTCHAT_CONTEXT")
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Native launch requires an absolute directory and preserves host home configuration.",
            ));
        }
        let executable = executable.canonicalize().map_err(|_| unavailable())?;
        native_executable(&executable)?;
        let original = std::fs::File::open(&executable).map_err(|_| unavailable())?;
        let metadata = original.metadata().map_err(|_| unavailable())?;
        let [stdin, stdout, stderr] = match terminal {
            NativeTerminal::Inherited => [Stdio::inherit(), Stdio::inherit(), Stdio::inherit()],
            NativeTerminal::Controlling => super::terminal::controlling_stdio()?,
        };
        let mut child = Command::new(&executable)
            .args(arguments)
            .current_dir(directory)
            // A new native conversation cannot inherit a caller's AGENTCHAT actor.
            .env_remove("AGENTCHAT_CONTEXT")
            .envs(environment)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| unavailable())?;
        let owner = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if child.try_wait().map_err(|_| unavailable())?.is_some() {
                    return Err(unavailable());
                }
                let pid = child.id().ok_or_else(unavailable)?;
                if let Ok(owner) = ProcessIdentity::capture(pid)
                    && owner.matches_executable(&executable, metadata.clone())
                    && owner.is_current()
                {
                    return Ok(owner);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| unavailable())??;
        let identity = LaunchOwners::capture(owner.pid(), std::process::id())?;
        if !owner.is_current() || LaunchOwners::observe(&identity) != LaunchOwnerState::Current {
            return Err(unavailable());
        }
        Ok(Self {
            child,
            owner,
            identity,
            startup_root: None,
        })
    }
    /// Original actual native image/start token. Never a shim, server or helper.
    pub fn owner(&self) -> &ProcessIdentity {
        &self.owner
    }
    /// Fresh owner/supervisor generation. It contains no conversation identity
    /// and supplies no trust, integration-ready or profile-admission proof.
    pub fn identity(&self) -> &LaunchIdentity {
        &self.identity
    }
    /// Requested pending barrier root, set before spawning by the managed
    /// constructor. This is selection, not native identity or ready evidence.
    pub fn startup_root(&self) -> Option<uuid::Uuid> {
        self.startup_root
    }
    /// Bounded kernel-only authority observation; Unknown never proves death.
    pub fn state(&self) -> LaunchOwnerState {
        LaunchOwners::observe(&self.identity)
    }
    /// Observe an already terminal child without consuming user input or issuing
    /// a synthetic native interruption. A live child still may be uninitialized.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child.try_wait().map_err(|_| unavailable())
    }
    /// Await native exit. The caller separately reconciles the exact original
    /// application generation; no owner/root transfer is implied by completion.
    pub async fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().await.map_err(|_| unavailable())
    }
    /// Request termination of this still-owned child and observe its actual
    /// exit. The unreaped Child keeps its PID from being reassigned. TERM gets
    /// a bounded grace period before forced shutdown; no probe error is treated
    /// as death and this operation never signals a supplied arbitrary PID.
    pub async fn terminate(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self.try_wait()? {
            return Ok(status);
        }
        let pid = self
            .child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(unavailable)?;
        if rustix::process::kill_process(pid, rustix::process::Signal::TERM).is_err() {
            return self.try_wait()?.ok_or_else(unavailable);
        }
        match tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await {
            Ok(status) => status.map_err(|_| unavailable()),
            Err(_) => {
                self.child.kill().await.map_err(|_| unavailable())?;
                self.wait().await
            }
        }
    }
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The native child exited or its original executable identity is unavailable.",
    )
}
