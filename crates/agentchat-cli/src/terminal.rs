//! Offline help/version presentation, before project discovery or runtime startup.

use clap::{Error, error::ErrorKind};
use std::io::{self, Write};

pub(crate) const BANNER: &str = include_str!("../assets/banner.txt");
const BRAND: craftguild_cli::Brand<'static> = craftguild_cli::Brand {
    banner: BANNER,
    project: "agentchat.rs",
};
include!(concat!(env!("OUT_DIR"), "/craftguild.rs"));

pub(crate) fn sbom() -> i32 {
    match craftguild_cli::sbom(&mut io::stdout().lock(), SBOM) {
        Ok(()) => 0,
        Err(_) => 8,
    }
}

/// Render untrusted finite text without terminal controls or bidi overrides.
pub(crate) fn escape(value: &str) -> String {
    let mut result = String::new();
    for c in value.chars() {
        if c.is_control() || matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') {
            use std::fmt::Write;
            write!(result, "\\u{{{:04x}}}", u32::from(c)).expect("string output");
        } else {
            result.push(c);
        }
    }
    result
}

pub(crate) fn display(error: &Error, json: bool, help: &str) -> i32 {
    let result = if error.kind() == ErrorKind::DisplayVersion && json {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(BUILD_INFO_JSON.as_bytes())
            .and_then(|_| stdout.flush())
    } else if error.kind() == ErrorKind::DisplayVersion {
        craftguild_cli::version(&mut io::stdout().lock(), BRAND, BUILD_INFO)
    } else if error.kind() == ErrorKind::DisplayHelp && error.to_string().trim() == help.trim() {
        craftguild_cli::help(&mut io::stdout().lock(), BRAND, help)
    } else {
        error.print()
    };
    if result.is_ok() { error.exit_code() } else { 8 }
}
