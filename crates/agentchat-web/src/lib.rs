//! Embedded, context-free viewer adapters. Persistence and runtime are injected.

mod assets;
mod connections;
mod context;
mod endpoint;
mod http;
mod projection;
pub mod render;
mod render_mentions;
mod resolvers;
mod server;

pub use context::ViewerContext;
pub use endpoint::endpoint;
pub use server::Viewer;
