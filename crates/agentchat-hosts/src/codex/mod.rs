//! Version-coupled shared app-server access, without creating another server.
mod adapter;
mod client;
pub use adapter::NativeAdapter;
mod discovery;
mod integration;
pub use integration::integration_namespace;
mod proxy;
pub use proxy::{Proxy, ProxyDecision, ProxyEvent, ProxyEvents, ProxyFence};
mod ready;
pub use ready::CodexReady;
mod registry;
pub use registry::{NativeRegistry, NativeRoute};
mod startup;
mod state;
pub use client::Client;
pub use discovery::Daemon;

#[cfg(test)]
#[path = "../../tests/unit/codex_delivery.rs"]
mod delivery_tests;

use agentchat_core::{Error, ErrorCode};

fn unavailable() -> Error {
    let mut error = Error::new(
        ErrorCode::RuntimeUnavailable,
        "The exact shared Codex control connection is unavailable.",
    );
    error.retryable = true;
    error
}
fn invalid() -> Error {
    Error::new(
        ErrorCode::UnsupportedProtocol,
        "The shared Codex response does not match the selected protocol.",
    )
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The exact launch-owned Codex conversation is unavailable or unconfirmed.",
    )
}
fn thread_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)
}
