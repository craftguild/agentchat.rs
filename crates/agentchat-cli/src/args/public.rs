//! Public actor syntax and leaf-only finite pagination.

use super::{Command, NativeHost, cursor};
use agentchat_core::{Error, ErrorCode, Result, read::PageRequest, sequence::Cursor};
use clap::Args;

#[derive(Debug, Default, Args)]
pub(crate) struct ActorArgs {
    /// Exact lowercase native host; requires --by-agent-id.
    #[arg(
        long,
        global = true,
        value_enum,
        requires = "by_agent_id",
        conflicts_with = "by_user"
    )]
    pub by_host: Option<NativeHost>,
    /// Exact opaque native chat ID; requires --by-host.
    #[arg(long, global = true, requires = "by_host", conflicts_with = "by_user")]
    pub by_agent_id: Option<String>,
    /// Select this project's common anonymous user.
    #[arg(long, global = true, conflicts_with_all = ["by_host", "by_agent_id"])]
    pub by_user: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActorMode {
    Required,
    Optional,
    Independent,
}

impl ActorArgs {
    pub fn is_selected(&self) -> bool {
        self.by_user || self.by_host.is_some() || self.by_agent_id.is_some()
    }

    pub fn validate(&self, command: &Command) -> Result<()> {
        self.selector()?;
        match command.actor_mode() {
            ActorMode::Required if !self.is_selected() => {
                return Err(invalid(
                    "Select --by-host HOST --by-agent-id CHAT_ID or --by-user.",
                ));
            }
            ActorMode::Independent if self.is_selected() => {
                return Err(invalid(
                    "Actor selectors are not applicable to this command.",
                ));
            }
            _ => {}
        }
        if self.by_user && matches!(command, Command::Join { name: Some(_) }) {
            return Err(invalid("Anonymous user Join does not accept --name."));
        }
        Ok(())
    }

    pub fn selector(&self) -> Result<Option<agentchat_core::model::ActorSelector>> {
        use agentchat_core::model::ActorSelector;
        let selector = match (self.by_host, &self.by_agent_id, self.by_user) {
            (None, None, false) => None,
            (None, None, true) => Some(ActorSelector::User),
            (Some(host), Some(chat_id), false) => Some(ActorSelector::Native {
                host: host.into(),
                chat_id: chat_id.clone(),
            }),
            _ => {
                return Err(invalid(
                    "Choose one complete native actor pair or --by-user.",
                ));
            }
        };
        if let Some(selected) = &selector {
            selected.validate()?;
        }
        Ok(selector)
    }
}

impl Command {
    pub fn actor_mode(&self) -> ActorMode {
        match self {
            Self::Sbom
            | Self::Viewer
            | Self::Launch { .. }
            | Self::Mcp
            | Self::Init { .. }
            | Self::Uninstall { .. }
            | Self::Internal { .. } => ActorMode::Independent,
            Self::Doctor
            | Self::Agents { .. }
            | Self::Message { .. }
            | Self::History { .. }
            | Self::Search { .. }
            | Self::Events { .. }
            | Self::Channel {
                command: super::Channel::List { .. } | super::Channel::Members { .. },
            }
            | Self::Reservations {
                command: super::Reservations::List { .. },
            } => ActorMode::Optional,
            _ => ActorMode::Required,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Args)]
pub(crate) struct PageArgs {
    /// Maximum finite page items (default 50, range 1 through 200).
    #[arg(long, value_parser = limit)]
    pub limit: Option<usize>,
    /// Last consumed nonnegative decimal cursor (default 0).
    #[arg(long, value_parser = cursor, default_value = "0")]
    pub after: Cursor,
}

impl PageArgs {
    pub fn request(self) -> PageRequest {
        PageRequest {
            limit: self.limit,
            after: self.after,
        }
    }
}

fn limit(value: &str) -> std::result::Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=200).contains(n))
        .ok_or_else(|| "Page limits require an integer from 1 through 200.".into())
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}
