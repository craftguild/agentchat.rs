//! Pinned Rust AST checker for shared policy, independent of text and comments.
mod policy;
use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let result = env::args()
        .skip(1)
        .map(|name| {
            let source = fs::read_to_string(&name).map_err(|error| format!("{name}: {error}"))?;
            let identity = name.ends_with("agentchat-core/src/model/identity.rs");
            Ok((name, source, identity))
        })
        .collect::<Result<Vec<_>, String>>()
        .and_then(|sources| policy::check_files(&sources));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
