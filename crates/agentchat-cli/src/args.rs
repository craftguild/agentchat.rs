//! Concrete CLI grammar; domain decisions remain in shared operations.

use agentchat_core::{project::Scope, sequence::Cursor};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use uuid::Uuid;

#[path = "args/public.rs"]
mod public;
pub(crate) use public::{ActorArgs, PageArgs};

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(crate) enum ChannelStateArg {
    #[default]
    Active,
    Archived,
    All,
}
impl From<ChannelStateArg> for agentchat_core::read::ChannelListCondition {
    fn from(value: ChannelStateArg) -> Self {
        match value {
            ChannelStateArg::Active => Self::Active,
            ChannelStateArg::Archived => Self::Archived,
            ChannelStateArg::All => Self::All,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "agentchat",
    color = clap::ColorChoice::Never,
    version,
    about = "Project-scoped agent chat and cooperative reservations"
)]
pub(crate) struct Cli {
    /// Emit the versioned machine result instead of plain text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Discover an initialized project at this path.
    #[arg(long, global = true)]
    pub project: Option<PathBuf>,
    #[command(flatten)]
    pub actor: ActorArgs,
    /// Stable successful-mutation retry key.
    #[arg(long, global = true)]
    pub request_id: Option<Uuid>,
    #[command(subcommand)]
    pub command: Command,
}
pub(crate) fn cursor(value: &str) -> Result<Cursor, String> {
    Cursor::parse(value).map_err(|e| e.message)
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ScopeArg {
    Repository,
    Directory,
}
impl From<ScopeArg> for Scope {
    fn from(value: ScopeArg) -> Self {
        match value {
            ScopeArg::Repository => Self::Repository,
            ScopeArg::Directory => Self::Directory,
        }
    }
}
#[derive(Debug, Args)]
pub(crate) struct Body {
    /// Complete message text, passed literally.
    #[arg(long, conflicts_with = "stdin", allow_hyphen_values = true)]
    pub body: Option<String>,
    /// Read UTF-8 message text from stdin, bounded to 65,536 bytes.
    #[arg(long)]
    pub stdin: bool,
}
#[derive(Debug, Args)]
pub(crate) struct Filter {
    /// Optional channel slug.
    #[arg(long)]
    pub channel: Option<String>,
    /// Root message UUID; reply IDs are rejected.
    #[arg(long)]
    pub thread: Option<Uuid>,
}
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Write the exact embedded CycloneDX SBOM without project discovery.
    Sbom,
    /// Open a loopback chat viewer with explicit common-user actions until Ctrl-C.
    Viewer,
    /// Start a native terminal with best-effort API-surface compatibility.
    Launch {
        #[arg(value_enum)]
        host: NativeHost,
        /// Resume this exact native conversation with a fresh launch generation.
        #[arg(long)]
        resume: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Serve typed MCP tools on stdin/stdout using verified native routing.
    Mcp,
    /// Create/recover project storage and install preserving host integrations.
    #[command(name = "install")]
    Init {
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        /// Ordered mandatory defaults as one comma-separated list.
        #[arg(long)]
        default_channels: Option<String>,
        /// Automatically record reading of complete native message bodies.
        #[arg(long, action = clap::ArgAction::Set)]
        auto_read: Option<bool>,
        /// Positive whole-second processing timeout, e.g. 30s, 60m, 2h.
        #[arg(long)]
        delivery_confirmation_timeout: Option<String>,
        #[arg(long)]
        relocate: bool,
        #[arg(long)]
        new_project: bool,
    },
    /// Remove the effective project reference and its database/history.
    Uninstall {
        /// Remove user-wide integrations and all selected state/history; retain project references.
        #[arg(long)]
        global: bool,
    },
    /// Integrity, schema, SQLite, socket and host diagnostics without repair.
    Doctor,
    /// Explicitly invite this exact host conversation.
    Join {
        #[arg(long)]
        name: Option<String>,
    },
    /// Leave, cancel future deliveries and release owned reservations.
    Leave,
    /// Selected actor identity, participation, project settings and health.
    Whoami,
    /// Retained participant queries.
    Agents {
        #[command(subcommand)]
        command: Agents,
    },
    /// Create/list channels or inspect joined membership.
    Channel {
        #[command(subcommand)]
        command: Channel,
    },
    /// Post an immutable root message.
    Post {
        /// Target channel; omission resolves to the project's primary default.
        #[arg(long)]
        channel: Option<String>,
        #[command(flatten)]
        body: Body,
    },
    /// Reply directly to a root message.
    Reply {
        /// Mark exactly these complete bodies read atomically with the reply.
        #[arg(long = "read-message-id")]
        read_message_ids: Vec<Uuid>,
        root_message_id: Uuid,
        #[command(flatten)]
        body: Body,
    },
    /// Retrieve a complete message and a bounded receipt page.
    Message {
        message_id: Uuid,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Retrieve immutable messages in creation order.
    History {
        #[command(flatten)]
        page: PageArgs,
        #[command(flatten)]
        filter: Filter,
    },
    /// Case-sensitive literal substring search, without a query language.
    Search {
        query: String,
        #[command(flatten)]
        page: PageArgs,
        #[command(flatten)]
        filter: Filter,
    },
    /// Retrieve pending/processing deliveries without settling or reading.
    Inbox {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Mark exactly these messages read, preserving first-read times.
    Read {
        #[arg(required=true,num_args=1..)]
        message_ids: Vec<Uuid>,
    },
    /// Finite passive event retrieval.
    Events {
        #[command(flatten)]
        page: PageArgs,
        #[arg(long)]
        channel: Option<String>,
    },
    /// Atomically acquire a cooperative pattern group.
    Reserve {
        #[arg(required=true,num_args=1..)]
        patterns: Vec<String>,
        #[arg(long)]
        ttl: Option<u32>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// List, renew or release reservation groups.
    Reservations {
        #[command(subcommand)]
        command: Reservations,
    },
    /// Version-coupled integration entrypoints; not generic command execution.
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: Internal,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum NativeHost {
    Codex,
    Claude,
}
impl From<NativeHost> for agentchat_core::model::HostKind {
    fn from(host: NativeHost) -> Self {
        match host {
            NativeHost::Codex => Self::Codex,
            NativeHost::Claude => Self::Claude,
        }
    }
}
#[derive(Debug, Subcommand)]
pub(crate) enum Agents {
    /// List joined/left identities and effective presence.
    List {
        #[command(flatten)]
        page: PageArgs,
    },
}
#[derive(Debug, Subcommand)]
pub(crate) enum Channel {
    /// Create a channel without changing membership.
    Create { name: String },
    /// Join this participant to a non-default channel.
    Join { name: String },
    /// Leave this participant's non-default channel membership.
    Leave { name: String },
    /// List channels by their creation sequence and derived archive state.
    List {
        #[command(flatten)]
        page: PageArgs,
        /// Derived archive state evaluated at one captured server time.
        #[arg(long, value_enum, default_value_t)]
        state: ChannelStateArg,
    },
    /// List currently joined members, including disconnected identities.
    Members {
        name: String,
        #[command(flatten)]
        page: PageArgs,
    },
}
#[derive(Debug, Subcommand)]
pub(crate) enum Reservations {
    /// Active groups, or all retained states when --all is supplied.
    List {
        #[command(flatten)]
        page: PageArgs,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Renew an owned active group at its exact generation.
    Renew {
        reservation_id: Uuid,
        #[arg(long)]
        generation: i64,
    },
    /// Release an owned group at its exact generation.
    Release {
        reservation_id: Uuid,
        #[arg(long)]
        generation: i64,
    },
}
#[path = "args/internal.rs"]
mod internal;
pub(crate) use internal::{GuardMode, InputMode, Internal};
