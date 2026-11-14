//! Ordinary PATH command selection, without an installed-engine fallback.
use agentchat_core::{Error, ErrorCode, Result};
use std::{ffi::OsString, os::unix::fs::PermissionsExt, path::PathBuf};

pub(super) fn locate(program: &str, search: Option<OsString>) -> Result<PathBuf> {
    for directory in std::env::split_paths(&search.ok_or_else(|| missing(program))?) {
        let candidate = directory.join(program);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            std::env::current_dir()
                .map_err(|_| inaccessible(program))?
                .join(candidate)
        };
        match std::fs::metadata(&candidate) {
            Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => {
                return candidate.canonicalize().map_err(|_| inaccessible(program));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(_) => return Err(inaccessible(program)),
        }
    }
    Err(missing(program))
}

fn missing(program: &str) -> Error {
    let mut error = Error::new(
        ErrorCode::UnsupportedHost,
        format!("The {program} executable was not found on the current user's PATH."),
    );
    error.details = serde_json::json!({"command":program,"reason":"not_found_in_path"});
    error
}
fn inaccessible(program: &str) -> Error {
    let mut error = Error::new(
        ErrorCode::UnsupportedHost,
        format!("The {program} executable on the current user's PATH could not be inspected."),
    );
    error.details = serde_json::json!({"command":program,"reason":"path_entry_unavailable"});
    error
}
