//! Native-only metadata input to the isolated control channel.
use super::protocol::{
    self, Control, LifecycleOwner, NativeProfile, Request, Response, Result, Snapshot,
};
use agentchat_hosts::claude::{NativeCall, process::ProcessIdentity};
use agentchat_protocol::framing;
use std::{io::Read, path::Path, time::Duration};
use tokio::{io::BufReader, net::UnixStream};

pub(super) async fn run(root: &Path, mode: &str) -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin().take(65_537).read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err("native input exceeds 64 KiB".into());
    }
    let native: Snapshot = serde_json::from_slice(&bytes)?;
    if mode == "promotion-needed" {
        println!(
            "{}",
            super::lifecycle::promotion_needed(root, &native).await?
        );
        return Ok(());
    }
    let path = protocol::registration(root, &native.observer)?;
    let control = match mode {
        "register" => {
            let registration = super::lifecycle::register(root, &native).await?;
            protocol::write(
                &protocol::current_owner(root, native.owner_root),
                &registration,
            )?;
            Control::Register {
                registration: Box::new(registration),
            }
        }
        "record" | "refresh" => {
            let registration: LifecycleOwner = protocol::read(&path)?;
            let profile: NativeProfile = protocol::read(&root.join("native-profile.json"))?;
            let owner = ProcessIdentity::parent(&profile.executable)?;
            if registration.registration.root() != native.root
                || registration.registration.generation() != native.generation
                || registration.registration.owner_root() != native.owner_root
                || registration.owner != owner
            {
                return Err("native call scope changed".into());
            }
            if mode == "refresh" {
                Control::Refresh {
                    observer: native.observer,
                    generation: native.generation,
                    root: native.root,
                }
            } else {
                let call = NativeCall {
                    observer: native.observer,
                    generation: native.generation,
                    root: native.root,
                    sequence: native.sequence.ok_or("native sequence required")?,
                    tool_name: native.tool_name.ok_or("native tool required")?,
                    tool_use_id: native.tool_use_id.ok_or("native ID required")?,
                };
                Control::Record {
                    registration: Box::new(registration),
                    call,
                }
            }
        }
        "revoke" => Control::Revoke {
            observer: native.observer,
            generation: native.generation,
            root: native.root,
            tool_use_id: native.tool_use_id.ok_or("native ID required")?,
        },
        "retire" => Control::Retire {
            observer: native.observer,
            generation: native.generation,
            root: native.root,
        },
        _ => return Err("unknown hook mode".into()),
    };
    let path = protocol::endpoint(root, native.owner_root);
    // Native SessionEnd can follow MCP EOF. EOF already retires every
    // attachment, so a cleanup notice for a gone owner is idempotent. Missing
    // call/registration authority must still refuse ordinary dispatch.
    let cleanup = matches!(mode, "retire" | "revoke");
    if !path.exists() && (mode == "register" || cleanup) {
        return Ok(());
    }
    let endpoint: protocol::Endpoint = protocol::read(&path)?;
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let stream = match UnixStream::connect(endpoint.socket).await {
            Ok(stream) => stream,
            Err(error)
                if cleanup
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
            {
                return Ok(Response {
                    accepted: true,
                    error: None,
                });
            }
            Err(error) => return Err(error.into()),
        };
        let (reader, mut writer) = stream.into_split();
        framing::write(
            &mut writer,
            &Request {
                nonce: endpoint.nonce,
                control,
            },
        )
        .await?;
        let result = framing::read::<_, Response>(&mut BufReader::new(reader))
            .await?
            .ok_or("control EOF")?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(result)
    })
    .await??;
    if !response.accepted {
        return Err(format!("native control refused: {:?}", response.error).into());
    }
    Ok(())
}
