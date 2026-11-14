//! Bind offline presentation to the exact prepared application distribution.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let package =
        std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").ok_or("package")?);
    craftguild_cli_build::emit(&package.join("../.."), "agentchat")?;
    Ok(())
}
