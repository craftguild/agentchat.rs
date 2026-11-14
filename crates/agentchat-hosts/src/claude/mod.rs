//! Native Claude correlation, kernel ownership and freshness; no host admission.

pub mod adapter;
pub mod body;
mod calls;
pub mod freshness;
mod lifecycle;
pub mod metadata;
pub mod module;
pub mod observer;
pub mod process;
mod replay;
pub mod routing;
pub(crate) mod signal;

pub use calls::{CallProofs, CallScope, NativeCall, NativeRegistration};
pub use lifecycle::{NativeBootstrap, NativeLifecycle, NativeScope};

pub mod namespace;
pub mod wake;
