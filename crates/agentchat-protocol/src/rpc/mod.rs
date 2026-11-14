//! JSON-RPC 2.0 calls, bounded local client and finite observer transport.

mod client;
mod reads;
mod watch;
mod wire;

pub use client::Client;
pub use client::success;
pub use reads::ReadResult;
pub use watch::*;
pub use wire::*;

use agentchat_core::{Error, ErrorCode};
pub(crate) fn unavailable(message: &str) -> Error {
    let mut error = Error::new(ErrorCode::RuntimeUnavailable, message);
    error.retryable = true;
    error
}
pub(crate) fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::UnsupportedProtocol, message)
}
