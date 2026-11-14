//! CLI composition and literal input/output adaptation over shared operations.

pub mod claude_gateway;
pub mod codex_gateway;

mod adapt;
mod args;
mod duration;
mod input;
mod launch;
mod mcp;
mod native;
mod presentation;
mod process;
mod terminal;
mod viewer;

pub use process::operations;

use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::{
    envelope::{Envelope, Outcome},
    operations::{Backend, Target},
};
use agentchat_runtime::project::ProjectManager;
use args::Cli;
use clap::{CommandFactory, Parser};
use serde_json::Value;
use std::ffi::OsString;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Execute one concrete CLI command and return its documented exit category.
pub async fn run(arguments: impl IntoIterator<Item = OsString>) -> i32 {
    let mut arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.len() == 1 {
        arguments.push("--help".into());
    }
    let json = arguments.iter().any(|s| s == "--json");
    let stdio = protocol_arguments(&arguments);
    let generated = Uuid::new_v4();
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            let help = Cli::command().render_help().to_string();
            return terminal::display(&error, json, &help);
        }
        Err(error) => {
            if stdio {
                return mcp::failure(Error::new(ErrorCode::InvalidArgument, error.to_string()))
                    .await;
            }
            return output(
                &Envelope::new(
                    generated,
                    None,
                    Err(Error::new(ErrorCode::InvalidArgument, error.to_string())),
                ),
                json,
            )
            .await;
        }
    };
    let json = cli.json;
    let stdio = protocol_command(&cli.command);
    if let Err(error) = cli.actor.validate(&cli.command) {
        if stdio {
            return mcp::failure(error).await;
        }
        return output(
            &Envelope::new(cli.request_id.unwrap_or(generated), None, Err(error)),
            json,
        )
        .await;
    }
    if matches!(&cli.command, args::Command::Launch { .. }) {
        return launch::run(cli, generated).await;
    }
    if matches!(&cli.command, args::Command::Sbom) {
        return terminal::sbom();
    }
    if matches!(&cli.command, args::Command::Viewer) {
        return viewer::run(cli, generated).await;
    }
    if let args::Command::Mcp = cli.command {
        return mcp::run().await;
    }
    if let args::Command::Internal {
        command:
            command @ (args::Internal::ClaudeRelay { .. }
            | args::Internal::ClaudeAnswer { .. }
            | args::Internal::ClaudeNamespace { .. }
            | args::Internal::ClaudeControl { .. }
            | args::Internal::ClaudeStream { .. }
            | args::Internal::ClaudeGuard { .. }
            | args::Internal::ClaudeInput { .. }
            | args::Internal::ClaudeStartup { .. }),
    } = &cli.command
    {
        return native::run(command).await;
    }
    let id = cli.request_id.unwrap_or(generated);
    let result = execute(cli, id).await;
    match result {
        Ok(Some(envelope)) => output(&envelope, json).await,
        Ok(None) => 0,
        Err(error) => output(&Envelope::new(id, None, Err(error)), json).await,
    }
}
async fn execute(cli: Cli, id: Uuid) -> Result<Option<Envelope<Value>>> {
    let actor = cli.actor.selector()?;
    let action = adapt::action(cli.command).await?;
    let global = matches!(action, adapt::Action::Uninstall { global: true });
    let target = Target {
        expected_project_id: None,
        initialization_identity: None,
        project_path: match cli.project {
            _ if global => None,
            Some(path) => Some(path),
            None => Some(std::env::current_dir().map_err(|_| {
                Error::new(
                    ErrorCode::IoError,
                    "Current directory is unavailable; pass --project explicitly.",
                )
            })?),
        },
        actor,
        context_id: None,
    };
    let manager = ProjectManager::from_environment()?;
    if let adapt::Action::Daemon { project_id } = action {
        process::daemon(
            manager,
            target.project_path.as_deref().expect("explicit CLI path"),
            project_id,
        )
        .await?;
        return Ok(None);
    }
    let executable = process::current_executable()?;
    let operations = process::operations(manager, executable);
    match action {
        adapt::Action::Uninstall { global } => {
            let scope = if global {
                agentchat_protocol::removal::RemovalScope::Global
            } else {
                agentchat_protocol::removal::RemovalScope::Project
            };
            let envelope = operations.uninstall(id, scope, target.project_path).await;
            Ok(Some(envelope.map(|result| {
                serde_json::to_value(result).expect("removal DTO is JSON serializable")
            })))
        }
        adapt::Action::Finite(operation) => {
            Ok(Some(operations.invoke(id, target, operation).await))
        }
        adapt::Action::Daemon { .. } => {
            unreachable!("daemon is dispatched before public operations")
        }
        adapt::Action::Mcp => unreachable!("MCP is dispatched before CLI project discovery"),
    }
}
async fn output(envelope: &Envelope<Value>, json: bool) -> i32 {
    let code = match &envelope.outcome {
        Outcome::Success { .. } => 0,
        Outcome::Failure { error } => error.code.exit_code(),
    };
    if json {
        if agentchat_protocol::framing::write(&mut tokio::io::stdout(), envelope)
            .await
            .is_err()
        {
            return 8;
        }
    } else {
        let text = presentation::render(envelope);
        let mut writer: Box<dyn tokio::io::AsyncWrite + Unpin> = if code == 0 {
            Box::new(tokio::io::stdout())
        } else {
            Box::new(tokio::io::stderr())
        };
        if writer.write_all(text.as_bytes()).await.is_err() || writer.flush().await.is_err() {
            return 8;
        }
    }
    code
}

// Inspect only the root entrypoint before Clap can return a syntax error. A
// literal body/chat/path named "mcp" cannot select protocol error presentation.
fn protocol_arguments(arguments: &[OsString]) -> bool {
    let mut words = arguments.iter().skip(1);
    let mut internal = false;
    while let Some(word) = words.next() {
        if matches!(
            word.to_str(),
            Some("--project" | "--by-host" | "--by-agent-id" | "--request-id")
        ) {
            words.next();
            continue;
        }
        if word.to_str().is_some_and(|s| s.starts_with('-')) {
            continue;
        }
        if !internal && word == "internal" {
            internal = true;
            continue;
        }
        return if internal {
            matches!(
                word.to_str(),
                Some(
                    "claude-relay"
                        | "claude-answer"
                        | "claude-namespace"
                        | "claude-control"
                        | "claude-stream"
                        | "claude-guard"
                        | "claude-input"
                        | "claude-startup"
                )
            )
        } else {
            word == "mcp"
        };
    }
    false
}
fn protocol_command(command: &args::Command) -> bool {
    matches!(
        command,
        args::Command::Mcp
            | args::Command::Internal {
                command: args::Internal::ClaudeRelay { .. }
                    | args::Internal::ClaudeAnswer { .. }
                    | args::Internal::ClaudeNamespace { .. }
                    | args::Internal::ClaudeControl { .. }
                    | args::Internal::ClaudeStream { .. }
                    | args::Internal::ClaudeGuard { .. }
                    | args::Internal::ClaudeInput { .. }
                    | args::Internal::ClaudeStartup { .. }
            }
    )
}
