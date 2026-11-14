//! Host adapters and configuration integration for exact conversation bindings.
//!
//! Compatibility is best-effort: adapters require the APIs they consume and
//! report a missing live surface without treating versions as an allowlist.

mod admission;
pub use admission::{
    AdmittedHosts, capabilities, capability_report, missing_api, require_native_launch,
    require_native_profile,
};
pub mod claude;
pub mod codex;
mod diagnostics;
pub mod launch;
mod lock;
pub use diagnostics::inspect;
pub mod install;
pub mod lifecycle;
pub mod setup;
pub mod staging;

#[cfg(test)]
#[path = "../tests/support/device_metadata.rs"]
mod device_metadata;
