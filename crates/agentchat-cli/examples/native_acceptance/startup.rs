//! Exact-child observation/managed startup; never supplies a model prompt.
use super::Result;
use agentchat_hosts::{
    claude::{adapter::NativeAdapter, metadata},
    launch::{ClaudeStartup, NativeChild},
};
use agentchat_runtime::project::ProjectManager;
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use uuid::Uuid;

#[derive(clap::Args)]
pub(super) struct Inputs {
    pub state: PathBuf,
    pub project: PathBuf,
    pub native: PathBuf,
    pub config: PathBuf,
    pub profile: PathBuf,
    pub plugin: PathBuf,
    pub settings: PathBuf,
    pub report: PathBuf,
    #[arg(long)]
    pub root: Uuid,
    #[arg(long)]
    pub version: String,
    /// Exact installed agentchat required for owned bundle verification before managed startup.
    #[arg(long)]
    pub agentchat: Option<PathBuf>,
    #[arg(long)]
    pub mcp_config: Option<PathBuf>,
    /// Use a running explicit candidate runtime and shared managed startup.
    #[arg(long)]
    pub start_launch: bool,
    #[arg(long)]
    pub name: Option<String>,
    /// Explicit fixture coordination; never skips a native/core check.
    #[arg(long)]
    pub wait_before_start: Option<PathBuf>,
    /// Native exact-root resume with a fresh physical launch generation.
    #[arg(long)]
    pub resume: bool,
    /// Use normal native configuration discovery, without settings/MCP flags.
    #[arg(long)]
    pub configured: bool,
    /// Keep parent stdout separate from the native controlling terminal.
    #[arg(long)]
    pub terminal_json: bool,
}
pub(super) async fn run(input: Inputs) -> Result<()> {
    if input.start_launch && !input.configured {
        return Err(
            "Managed readiness requires the shared configured supervisor; use --configured".into(),
        );
    }
    if input.wait_before_start.is_some() {
        return Err(
            "The retired pre-Join startup hold is unsupported; observe unjoined readiness instead"
                .into(),
        );
    }
    if input.terminal_json && !input.configured {
        return Err("Controlling-terminal acceptance requires configured startup".into());
    }
    if input.configured && !input.start_launch {
        return Err("Configured startup requires managed owned-bundle verification".into());
    }
    if input.start_launch
        && !agentchat_hosts::claude::module::has_startup_gate_source(&input.version)
    {
        return Err("The exact candidate has no managed first-input gate".into());
    }
    let project = input.project.canonicalize()?;
    let manager = ProjectManager::new(input.state.clone())?;
    let initialized = manager
        .discover(&project)
        .await?
        .ok_or("Initialize the disposable project first")?;
    let mut configured_plan = None;
    if input.start_launch {
        let agentchat = input
            .agentchat
            .as_ref()
            .ok_or("Managed startup requires --agentchat for owned bundle verification")?;
        let plan = agentchat_hosts::setup::ToolSetup::new(
            manager.home().into(),
            agentchat.clone(),
            agentchat_hosts::setup::Environment {
                user_config_root: Some(input.config.clone()),
                claude_config_dir: input.settings.parent().map(PathBuf::from),
                ..Default::default()
            },
            std::env::var_os("PATH"),
        )
        .claude_launch_plan()
        .await?;
        // The parent directory may also contain explicit acceptance-only audit
        // plugins. Every AGENTCHAT path still comes from the single owned snapshot;
        // raw fixture flags cannot select a different native bundle or hook.
        if plan.profile().executable != input.native
            || plan.profile().version != input.version
            || (plan.plugin_directory() != input.plugin
                && plan.plugin_directory().parent() != Some(input.plugin.as_path()))
            || plan.settings() != input.settings
            || (!input.configured && input.mcp_config.as_deref() != Some(plan.registry()))
        {
            return Err("Managed startup paths disagree with the owned native bundle".into());
        }
        if input.configured {
            if input.mcp_config.is_some()
                || plan.settings().parent() != Some(input.profile.as_path())
                || plan.plugin_directory() != input.plugin
            {
                return Err(
                    "Configured startup must use the owned native configuration directory".into(),
                );
            }
            plan.configured_environment()?;
            configured_plan = Some(plan);
        }
    }
    let startup = ClaudeStartup::new(
        input.root,
        initialized.identity().id,
        project.clone(),
        input.version.clone(),
    )?;
    for path in [
        &input.config,
        &input.profile,
        &input.plugin,
        &input.settings,
        &input.report,
    ] {
        if !path.is_absolute() {
            return Err("Explicit absolute fixture paths required".into());
        }
    }
    if let Some(plan) = configured_plan {
        if input.wait_before_start.is_some() {
            return Err("Configured supervision does not accept the explicit fixture hold".into());
        }
        return super::configured_supervisor::run(&manager, &initialized, project, &plan, &input)
            .await;
    }
    let environment = BTreeMap::from([
        ("AGENTCHAT_HOME".into(), manager.home().as_os_str().into()),
        (
            "AGENTCHAT_USER_CONFIG_ROOT".into(),
            input.config.into_os_string(),
        ),
        ("CLAUDE_CONFIG_DIR".into(), input.profile.into_os_string()),
    ]);
    let mut arguments = vec![
        if input.resume {
            "--resume".into()
        } else {
            "--session-id".into()
        },
        input.root.to_string().into(),
        "--plugin-dir".into(),
        input.plugin.into_os_string(),
    ];
    if !input.configured {
        arguments.extend(["--settings".into(), input.settings.into_os_string()]);
    }
    if let Some(config) = input.mcp_config {
        if !config.is_absolute() {
            return Err("Explicit absolute MCP configuration required".into());
        }
        arguments.extend(["--mcp-config".into(), config.into_os_string()]);
    }
    let mut child = if input.start_launch {
        NativeChild::spawn_with_startup_gate(
            &input.native,
            &project,
            &arguments,
            &environment,
            input.root,
        )
        .await?
    } else {
        NativeChild::spawn(&input.native, &project, &arguments, &environment).await?
    };
    let owner = child.owner().clone();
    metadata::write(
        &input.report.with_extension("spawn.json"),
        &json!({
            "owner":owner,"launch":child.identity(),"spawned":true,"startup_observed":false,
            "membership_established":false,"admitted":false,
        }),
    )?;
    let adapter = NativeAdapter::new(manager.home().into());
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("Native exited before startup: {status}").into());
        }
        if let Some(ready) = startup.observe(manager.home(), &child, &adapter).await? {
            metadata::write(
                &input.report,
                &json!({
                    "owner":owner,"launch":child.identity(),"startup_observed":true,
                    "native_generation":ready.native_generation(),"context":ready.context(),
                    "phase":ready.state().phase,"wake_paused":ready.state().wake_paused,
                    "membership_established":false,"admitted":false,
                }),
            )?;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status = child.wait().await?;

    metadata::write(
        &input.report.with_extension("exit.json"),
        &json!({
            "owner":owner,"exit_code":status.code(),"success":status.success(),
            "original_owner_gone":child.state()==agentchat_core::model::LaunchOwnerState::Gone,
            "admitted":false,
        }),
    )?;
    if !status.success() {
        return Err(format!("Native exit: {status}").into());
    }
    Ok(())
}
