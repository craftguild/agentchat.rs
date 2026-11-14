//! CLI syntax adaptation to existing typed application operations.

use crate::{args::*, input};
use agentchat_core::{Result, application::Mutation, read::Query};
use agentchat_protocol::operations::Operation;
use uuid::Uuid;

pub(crate) enum Action {
    Mcp,
    Finite(Operation),
    Uninstall { global: bool },
    Daemon { project_id: Uuid },
}
pub(crate) async fn action(command: Command) -> Result<Action> {
    let operation = match command {
        Command::Sbom | Command::Launch { .. } | Command::Viewer => {
            unreachable!("interactive launch is dispatched before finite operations")
        }
        Command::Mcp => return Ok(Action::Mcp),
        Command::Uninstall { global } => return Ok(Action::Uninstall { global }),
        Command::Internal {
            command:
                Internal::ClaudeRelay { .. }
                | Internal::ClaudeAnswer { .. }
                | Internal::ClaudeNamespace { .. }
                | Internal::ClaudeControl { .. }
                | Internal::ClaudeStream { .. }
                | Internal::ClaudeGuard { .. }
                | Internal::ClaudeInput { .. }
                | Internal::ClaudeStartup { .. },
        } => {
            unreachable!("native streams are dispatched before finite CLI operations")
        }
        Command::Init {
            scope,
            default_channels,
            auto_read,
            delivery_confirmation_timeout,
            relocate,
            new_project,
        } => Operation::Initialize {
            scope: scope.map(Into::into),
            default_channels: default_channels
                .map(|value| value.split(',').map(str::to_owned).collect::<Vec<String>>()),
            auto_read,
            delivery_confirmation_timeout_seconds: crate::duration::seconds(
                delivery_confirmation_timeout.as_deref(),
            )?,
            relocate,
            new_project,
        },
        Command::Doctor => Operation::Doctor,
        Command::Whoami => query(Query::Whoami),
        Command::Join { name } => mutate(Mutation::Join { name }),
        Command::Leave => mutate(Mutation::Leave),
        Command::Agents {
            command: Agents::List { page },
        } => query(Query::Agents {
            page: page.request(),
        }),
        Command::Channel { command } => match command {
            Channel::Create { name } => mutate(Mutation::ChannelCreate { name }),
            Channel::Join { name } => mutate(Mutation::ChannelJoin { name }),
            Channel::Leave { name } => mutate(Mutation::ChannelLeave { name }),
            Channel::List { state, page } => query(Query::Channels {
                condition: state.into(),
                page: page.request(),
            }),
            Channel::Members { name, page } => query(Query::ChannelMembers {
                channel: name,
                page: page.request(),
            }),
        },
        Command::Post { channel, body } => mutate(Mutation::Post {
            channel,
            body: input::body(body).await?,
        }),
        Command::Reply {
            read_message_ids,
            root_message_id,
            body,
        } => mutate(Mutation::Reply {
            read_message_ids,
            root_message_id,
            body: input::body(body).await?,
        }),
        Command::Message { message_id, page } => query(Query::Message {
            message_id,
            receipts: page.request(),
        }),
        Command::History { filter, page } => query(Query::History {
            channel: filter.channel,
            thread: filter.thread,
            page: page.request(),
        }),
        Command::Search {
            query: term,
            filter,
            page,
        } => query(Query::Search {
            query: term,
            channel: filter.channel,
            thread: filter.thread,
            page: page.request(),
        }),
        Command::Inbox { page } => query(Query::Inbox {
            page: page.request(),
        }),
        Command::Read { message_ids } => mutate(Mutation::Read { message_ids }),
        Command::Events { channel, page } => query(Query::Events {
            channel,
            page: page.request(),
        }),
        Command::Reserve {
            patterns,
            ttl,
            reason,
        } => mutate(Mutation::Reserve {
            patterns,
            ttl_seconds: ttl,
            reason,
        }),
        Command::Reservations { command } => match command {
            Reservations::List { owner, all, page } => query(Query::Reservations {
                owner,
                all,
                page: page.request(),
            }),
            Reservations::Renew {
                reservation_id,
                generation,
            } => mutate(Mutation::ReservationsRenew {
                reservation_id,
                generation,
            }),
            Reservations::Release {
                reservation_id,
                generation,
            } => mutate(Mutation::ReservationsRelease {
                reservation_id,
                generation,
            }),
        },
        Command::Internal {
            command: Internal::Daemon { project_id },
        } => return Ok(Action::Daemon { project_id }),
    };
    Ok(Action::Finite(operation))
}
fn query(query: Query) -> Operation {
    Operation::Query { query }
}
fn mutate(command: Mutation) -> Operation {
    Operation::Mutate { command }
}
