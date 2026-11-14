//! Native-only grammar, independent of public application commands.
use clap::Subcommand;
use std::path::PathBuf;
use uuid::Uuid;
#[derive(Debug, Subcommand)]
pub(crate) enum Internal {
    /// Wait for this exact managed native child's first-input startup barrier.
    ClaudeStartup {
        #[arg(long)]
        native_executable: PathBuf,
    },
    /// Consume or record one exact full native input; never a model tool.
    ClaudeInput {
        #[arg(long)]
        native_executable: PathBuf,
        #[arg(long)]
        state_directory: PathBuf,
        #[arg(long, value_enum)]
        mode: InputMode,
    },
    /// Validate native notice/final boundaries without a model operation.
    ClaudeGuard {
        #[arg(long)]
        native_executable: PathBuf,
        #[arg(long)]
        state_directory: PathBuf,
        #[arg(long, value_enum)]
        mode: GuardMode,
    },
    /// Native-owned observation/transport-renewal stream in its fixed owner scope.
    ClaudeStream {
        #[arg(long)]
        native_executable: PathBuf,
        /// Explicit native state location; does not depend on spawn env support.
        #[arg(long)]
        state_directory: Option<PathBuf>,
        /// Pace replacement after a lost native observer helper.
        #[arg(long)]
        restart_backoff: bool,
    },
    /// Process one typed native callback using its exact direct parent.
    ClaudeControl {
        #[arg(long)]
        native_executable: PathBuf,
    },
    /// Resolve a private kernel-owner namespace, without a conversation identity.
    ClaudeNamespace {
        #[arg(long)]
        native_executable: PathBuf,
        // Compatibility for already-installed 2.1.301 modules, which predate
        // the explicit identity argument. Current modules always pass it.
        #[arg(long, default_value = "2.1.301")]
        native_version: String,
    },

    /// Native-owned observation stream; never application context authority.
    ClaudeRelay {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        observer: String,
        #[arg(long)]
        native_executable: PathBuf,
    },
    /// Return a native API snapshot through its exact short reply socket.
    ClaudeAnswer {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Run one OS-owned project runtime; the launch handshake checks this UUID.
    Daemon {
        #[arg(long)]
        project_id: Uuid,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum GuardMode {
    Notice,
    Final,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum InputMode {
    Load,
    Claim,
    Record,
    Notice,
    Final,
}
