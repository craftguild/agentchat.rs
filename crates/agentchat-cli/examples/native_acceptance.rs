//! Portable actual-native acceptance helper; never a production host admission.
//!
//! Every operation takes explicit disposable paths. Build this alongside `agentchat`
//! and use `--help`. Managed startup becomes ready while unjoined;
//! observation/planning never joins, and no command admits a profile.
#[path = "native_acceptance/acceptance_hold.rs"]
mod acceptance_hold;
#[path = "native_acceptance/boundary.rs"]
mod boundary;
#[path = "native_acceptance/candidate.rs"]
mod candidate;
#[path = "native_acceptance/codex.rs"]
mod codex;
#[path = "native_acceptance/configured_supervisor.rs"]
mod configured_supervisor;
#[path = "native_acceptance/install.rs"]
mod install;
#[path = "native_acceptance/startup.rs"]
mod startup;

use agentchat_hosts::claude::{
    freshness::Freshness, namespace::OwnerNamespace, observer::exchange, process::ProcessIdentity,
};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Parser)]
#[command(about = "Isolated exact-version Claude candidate verification; admits no profile")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve only the explicitly isolated actual Codex candidate, without admission.
    CodexServe {
        state: PathBuf,
        project: PathBuf,
        /// Exact native label selected by this disposable fixture; never admission.
        #[arg(long, default_value = "0.163.0")]
        version: String,
        /// Stop this disposable runtime after native acceptance, before Core commit.
        #[arg(long)]
        hold_accepted: Option<PathBuf>,
    },
    /// Run the product Codex supervisor with explicit fixture MCP configuration.
    CodexStartup(Box<codex::Inputs>),
    /// Inspect the physical birth of one explicitly test-owned process.
    Identity { pid: u32 },
    /// Plan native integration under CONFIG/host-config without changing files.
    Plan {
        root: PathBuf,
        config: PathBuf,
        native: PathBuf,
        /// Use a separately staged agentchat executable with the existing fixture state.
        #[arg(long)]
        agentchat: Option<PathBuf>,
        #[arg(long, default_value = "2.1.301")]
        version: String,
    },
    /// Install, repeat and inspect the isolated plan using the real installer.
    Install {
        root: PathBuf,
        config: PathBuf,
        native: PathBuf,
        /// Use a separately staged agentchat executable with the existing fixture state.
        #[arg(long)]
        agentchat: Option<PathBuf>,
        #[arg(long, default_value = "2.1.301")]
        version: String,
    },
    /// Inspect the intact owned native launch inputs without starting a host.
    LaunchPlan {
        state: PathBuf,
        config: PathBuf,
        agentchat: PathBuf,
    },
    /// Serve one initialized disposable project using the actual NativeAdapter.
    Serve {
        state: PathBuf,
        project: PathBuf,
        #[arg(long, default_value = "2.1.301")]
        version: String,
        /// Stop this disposable runtime after first native acceptance, before core commit.
        #[arg(long)]
        hold_accepted: Option<PathBuf>,
    },
    /// Challenge one test-owned native process without changing its lifecycle.
    Observe {
        state: PathBuf,
        native: PathBuf,
        pid: u32,
    },
    /// Challenge this helper's actual native parent while its test hook waits.
    /// Records observation only; a separate fixture hook blocks model input.
    ObserveParent {
        state: PathBuf,
        native: PathBuf,
        report: PathBuf,
        #[arg(long)]
        root: uuid::Uuid,
        #[arg(long)]
        check_membership: bool,
    },
    /// Supervise one actual native child and record its exact startup challenge.
    /// Supplies no model prompt, admission or trust/approval override.
    Startup(Box<startup::Inputs>),
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    match Args::parse().command {
        Command::CodexServe {
            state,
            project,
            version,
            hold_accepted,
        } => codex::serve(state, project, version, hold_accepted).await?,
        Command::CodexStartup(input) => codex::startup(*input).await?,
        Command::Identity { pid } => println!(
            "{}",
            serde_json::to_string(&ProcessIdentity::capture(pid)?)?
        ),
        Command::Plan {
            root,
            config,
            native,
            agentchat,
            version,
        } => install::run(root, config, native, agentchat, version, false).await?,
        Command::Install {
            root,
            config,
            native,
            agentchat,
            version,
        } => install::run(root, config, native, agentchat, version, true).await?,
        Command::LaunchPlan {
            state,
            config,
            agentchat,
        } => {
            let plan = agentchat_hosts::setup::ToolSetup::new(
                state,
                agentchat,
                agentchat_hosts::setup::Environment {
                    claude_config_dir: Some(config.join("host-config")),
                    user_config_root: Some(config),
                    ..Default::default()
                },
                std::env::var_os("PATH"),
            )
            .claude_launch_plan()
            .await?;
            println!(
                "{}",
                json!({"profile":plan.profile(),"managed_dispatch":plan.is_managed(),"plugin":plan.plugin_directory(),"settings":plan.settings(),"registry":plan.registry(),"native_ready":false,"admitted":false})
            );
        }
        Command::Serve {
            state,
            project,
            version,
            hold_accepted,
        } => candidate::serve(state, project, version, hold_accepted).await?,
        Command::Observe { state, native, pid } => observe(state, native, pid).await?,
        Command::ObserveParent {
            state,
            native,
            report,
            root,
            check_membership,
        } => boundary::observe(state, native, report, root, check_membership).await?,
        Command::Startup(input) => startup::run(*input).await?,
    }
    Ok(())
}

async fn observe(home: PathBuf, native: PathBuf, pid: u32) -> Result<()> {
    let native = native.canonicalize()?;
    let owner = ProcessIdentity::capture(pid)?;
    if owner.executable() != native {
        return Err("Native executable differs".into());
    }
    let namespace = OwnerNamespace::existing(&home, owner.clone()).await?;
    let before = namespace
        .current()
        .await?
        .ok_or("No native callback registration")?;
    let mut proof = Freshness::default();
    let request = proof.begin_lifecycle(&before.lifecycle, &owner, Duration::from_secs(5))?;
    let answer = exchange(namespace.directory(), namespace.observer(), pid, &request).await?;
    let after = namespace
        .current()
        .await?
        .ok_or("Native lifecycle retired")?;
    if before != after {
        return Err("Native lifecycle changed during observation".into());
    }
    let state = proof.finish_lifecycle(&after.lifecycle, &owner, answer.clone())?;
    println!(
        "{}",
        json!({
            "directory": namespace.directory(), "owner": owner, "registration": after,
            "snapshot": answer, "phase": state.phase, "wake_paused": state.wake_paused,
            "active_turn_id": state.active_turn_id, "admitted": false
        })
    );
    Ok(())
}
