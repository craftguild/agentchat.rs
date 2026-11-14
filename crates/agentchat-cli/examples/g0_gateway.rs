//! Isolated real-host root-routing probe; never admits or fakes a host profile.
#[path = "g0_gateway/candidate.rs"]
mod candidate;
#[path = "g0_gateway/fresh.rs"]
mod fresh;
#[path = "g0_gateway/hook.rs"]
mod hook;
#[path = "g0_gateway/lifecycle.rs"]
mod lifecycle;
#[path = "g0_gateway/protocol.rs"]
mod protocol;
#[path = "g0_gateway/server.rs"]
mod server;

use std::path::PathBuf;

#[tokio::main]
async fn main() -> protocol::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let mode = args.get(1).ok_or("supply probe mode and fixture root")?;
    let root = PathBuf::from(args.get(2).ok_or("supply fixture root")?).canonicalize()?;
    protocol::isolated(&root)?;
    match mode.as_str() {
        "candidate-runtime" => candidate::run(&root).await?,
        "observer-check" => {
            fresh::state(
                &root,
                args.get(3).ok_or("supply exact context UUID")?.parse()?,
            )
            .await?
        }
        "mcp" => {
            server::run(
                &root,
                PathBuf::from(args.get(3).ok_or("supply actual agentchat binary")?),
            )
            .await?
        }
        "register" | "record" | "revoke" | "retire" | "refresh" => hook::run(&root, mode).await?,
        "promotion-needed" => hook::run(&root, mode).await?,
        "owner-check" => {
            let observer = args.get(3).ok_or("supply the owned observer")?;
            let input: protocol::LifecycleOwner =
                protocol::read(&protocol::registration(&root, observer)?)?;
            println!(
                "{}",
                serde_json::json!({"root":input.registration.root(),"scope":format!("{:?}",input.registration.scope()),"owner_pid":input.owner.pid(),"owner_current":input.owner.is_current()})
            );
        }
        "fresh-check" => fresh::run(&root, args.get(3).ok_or("supply the owned observer")?).await?,
        _ => return Err("unknown isolated probe mode".into()),
    }
    Ok(())
}
