//! Best-effort host compatibility based on the APIs each adapter consumes.

use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Context, HostKind},
    ports::{HostGate, HostState},
};
use async_trait::async_trait;
use serde_json::{Value, json};

#[derive(serde::Serialize)]
struct Surface {
    host: HostKind,
    adapter: &'static str,
    required_api: &'static [&'static str],
}
const CODEX_API: &[&str] = &[
    "initialize.experimentalApi",
    "thread/start",
    "thread/resume",
    "thread/read",
    "thread/inject_items",
    "mcpServerStatus/list",
    "turn/start",
];
const CLAUDE_API: &[&str] = &[
    "session.id",
    "session.cwd",
    "session.version",
    "session.surfaces",
    "session.append",
    "command.register",
    "command.list",
    "command.run",
    "process.run",
    "process.spawn",
    "env.get",
];
const SURFACES: &[Surface] = &[
    Surface {
        host: HostKind::Codex,
        adapter: "codex-app-server",
        required_api: CODEX_API,
    },
    Surface {
        host: HostKind::Claude,
        adapter: "claude-native-plugin",
        required_api: CLAUDE_API,
    },
];
fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Require a compiled adapter for the requested host.
///
/// This is only the structural first step. The adapter checks each required API
/// when it connects to the live host and returns the missing surface to callers.
pub fn require_native_launch(host: HostKind) -> Result<()> {
    if SURFACES.iter().any(|surface| surface.host == host) {
        Ok(())
    } else {
        Err(unsupported(host, None, &platform(), &[]))
    }
}

/// Validate a selected host observation without using its version as admission.
///
/// The version remains an identity label: later live observations must match it
/// so one launch cannot mix different host processes. Compatibility is decided
/// by the adapter's required API calls rather than a semantic-version allowlist.
pub fn require_native_profile(host: HostKind, version: &str, os_profile: &str) -> Result<()> {
    require_native_launch(host)?;
    if os_profile == platform() && valid_version(version) {
        Ok(())
    } else {
        Err(unsupported(host, Some(version), os_profile, &[]))
    }
}

/// Report APIs required by an adapter but absent from the observed host.
pub fn missing_api(host: HostKind, version: Option<&str>, missing: &[&str]) -> Error {
    unsupported(host, version, &platform(), missing)
}

fn valid_version(version: &str) -> bool {
    !version.is_empty() && version.len() <= 128 && !version.chars().any(char::is_control)
}

/// Production host gate backed by live adapter API checks and runtime identity.
pub struct AdmittedHosts {
    native: crate::claude::adapter::NativeAdapter,
    codex: crate::codex::NativeAdapter,
    launches: crate::launch::Registry,
}
impl AdmittedHosts {
    /// Capture the state root without probing hosts, creating files or starting
    /// work. Every observation and presentation checks the selected host label;
    /// each adapter then validates the live API response it consumes.
    pub fn new(home: std::path::PathBuf) -> Self {
        Self {
            launches: crate::launch::Registry::new(home.clone()),
            native: crate::claude::adapter::NativeAdapter::new(home.clone()),
            codex: crate::codex::NativeAdapter::new(home),
        }
    }
}
#[async_trait]
impl HostGate for AdmittedHosts {
    fn input_capabilities(
        &self,
        context: &Context,
    ) -> Result<agentchat_core::delivery::InputCapabilities> {
        eligible(context)?;
        match context.host {
            HostKind::Codex => Ok(self.codex.input_capabilities()),
            HostKind::Claude => Ok(self.native.input_capabilities()),
        }
    }
    async fn launch_identity(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::model::LaunchIdentity>> {
        let Some(binding) = self.launches.resolve(context)? else {
            return Ok(None);
        };
        let Some(ready) = self.launches.readiness(&binding)? else {
            return Ok(None);
        };
        if self.launches.authorization(context)?.as_ref() != Some(&ready) {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "The original managed launch association is no longer current.",
            ));
        }
        Ok(Some(binding.identity))
    }
    async fn launch_delivery_identity(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::model::LaunchIdentity>> {
        let Some(identity) = self.launch_identity(context).await? else {
            return Ok(None);
        };
        let Some(binding) = self.launches.resolve(context)? else {
            return Ok(None);
        };
        if binding.identity != identity || !self.launches.was_started(&binding)? {
            return Ok(None);
        }
        Ok(Some(identity))
    }
    async fn launch_name(&self, context: &Context) -> Result<Option<String>> {
        let Some(binding) = self.launches.resolve(context)? else {
            return Ok(None);
        };
        Ok(self
            .launches
            .readiness(&binding)?
            .and_then(|ready| ready.preferred_name().map(str::to_owned)))
    }
    fn launch_owner(
        &self,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> agentchat_core::model::LaunchOwnerState {
        crate::launch::LaunchOwners::observe(identity)
    }
    fn launch_root_changed(
        &self,
        project: uuid::Uuid,
        host: agentchat_core::model::HostKind,
        root: &str,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> bool {
        self.launches.root_changed(project, host, root, identity)
    }
    fn launch_root_departed(
        &self,
        project: uuid::Uuid,
        host: agentchat_core::model::HostKind,
        root: &str,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> bool {
        self.launches.root_departed(project, host, root, identity)
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        eligible(context)?;
        match context.host {
            HostKind::Codex => self.codex.observe(context).await,
            HostKind::Claude => self.native.observe(context).await,
        }
    }
}
#[async_trait]
impl agentchat_core::delivery::DeliveryPort for AdmittedHosts {
    async fn present(
        &self,
        context: &Context,
        dispatch: &agentchat_core::delivery::WakeDispatch,
    ) -> Result<agentchat_core::delivery::HostReceipt> {
        eligible(context)?;
        self.native.present(context, dispatch).await
    }
}
#[async_trait]
impl agentchat_core::delivery::MessagePort for AdmittedHosts {
    async fn present_message(
        &self,
        context: &Context,
        dispatch: &agentchat_core::delivery::MessageDispatch,
    ) -> Result<agentchat_core::delivery::MessageProgress> {
        eligible(context)?;
        match context.host {
            HostKind::Codex => {
                agentchat_core::delivery::MessagePort::present_message(
                    &self.codex,
                    context,
                    dispatch,
                )
                .await
            }
            HostKind::Claude => {
                agentchat_core::delivery::MessagePort::present_message(
                    &self.native,
                    context,
                    dispatch,
                )
                .await
            }
        }
    }

    async fn present_notice(
        &self,
        context: &Context,
        dispatch: &agentchat_core::delivery::NoticeDispatch,
    ) -> Result<agentchat_core::delivery::MessageProgress> {
        eligible(context)?;
        match context.host {
            HostKind::Codex => {
                agentchat_core::delivery::MessagePort::present_notice(
                    &self.codex,
                    context,
                    dispatch,
                )
                .await
            }
            HostKind::Claude => {
                agentchat_core::delivery::MessagePort::present_notice(
                    &self.native,
                    context,
                    dispatch,
                )
                .await
            }
        }
    }
}
fn eligible(context: &Context) -> Result<()> {
    require_native_profile(context.host, &context.host_version, &context.os_profile)
}
fn unsupported(host: HostKind, version: Option<&str>, os_profile: &str, missing: &[&str]) -> Error {
    let mut error = Error::new(
        if missing.is_empty() {
            ErrorCode::UnsupportedHost
        } else {
            ErrorCode::UnsupportedProtocol
        },
        if missing.is_empty() {
            "The selected host observation cannot be used by this adapter."
        } else {
            "The host does not provide an API required by AgentChat."
        },
    );
    error.details = json!({
        "host":host,
        "host_version":version,
        "os_profile":os_profile,
        "compatibility":"best_effort_api_surface",
        "missing_api":missing,
    });
    error
}
/// Current best-effort compatibility record.
pub fn capabilities() -> Value {
    serde_json::to_value(capability_report()).expect("capability DTO is JSON serializable")
}
/// Typed diagnostic capability report for shared status composition.
pub fn capability_report() -> agentchat_protocol::integration::Capabilities {
    use agentchat_protocol::integration::{Capabilities, HostSurface};
    Capabilities {
        automatic_wake: true,
        compatibility: "best_effort_api_surface".into(),
        host_surfaces: SURFACES
            .iter()
            .map(|s| HostSurface {
                host: s.host,
                adapter: s.adapter.into(),
                required_api: s.required_api.iter().map(|a| (*a).into()).collect(),
            })
            .collect(),
        version_policy: "diagnostic_and_identity_only".into(),
    }
}
